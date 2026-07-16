// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(test))]
use slint_keyos_platform::file_backed::JsonBacked;
use {
    crate::{
        error::VaultError,
        fs_permissions::FileSystemPermissions,
        seed::{Seed, SeedDuplicateReason, SeedEditField, SeedType},
        AccountsParams, Animate, AppWindow, CallbackResult, Callbacks, GuiApi, Navigate, NavigateOptions,
        SeedView, SeedViewType,
    },
    anyhow::Context,
    bip85_extended::{
        bip39::Mnemonic,
        bitcoin::{
            bip32::Xpriv,
            secp256k1::{Secp256k1, SignOnly},
            Network,
        },
    },
    fuzzy_filter::FuzzyFilter,
    nostr::{nips::nip06::FromMnemonic, FromBech32, ToBech32},
    ordered_table::{CardSortMode, FilePersistence, OrderedTable, SortableCard},
    security::Seed as SecuritySeed,
    slint_keyos_platform::slint::{self, ComponentHandle, Image, ModelRc, SharedString, VecModel},
    std::{rc::Rc, sync::Arc},
    zeroize::{Zeroize, ZeroizeOnDrop},
};

security::use_api!();

pub const DATABASE_FILE: &str = "seed_vault_database_v2.json";

#[derive(Zeroize, ZeroizeOnDrop)]
pub enum PendingImport {
    NostrKey(String),
    ImportedSeed { entropy: String, fingerprint: String },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct VaultSettings {
    sort_mode: CardSortMode,
}

impl Default for VaultSettings {
    fn default() -> Self { Self { sort_mode: CardSortMode::Label } }
}

pub struct AppState {
    pub ui: slint::Weak<AppWindow>,
    pub gui: Arc<GuiApi>,
    seed_table: OrderedTable<Seed, FilePersistence<FileSystemPermissions>>,
    pub search_text: String,
    pub archive_mode: bool,
    secp: Secp256k1<SignOnly>,
    security_api: Security,
    model: Rc<VecModel<SeedView>>,
    pub pending_import: Option<PendingImport>,
    #[cfg(not(test))]
    settings: JsonBacked<VaultSettings, FileSystemPermissions>,
    #[cfg(test)]
    sort_mode: CardSortMode,
}

impl AppState {
    pub fn new(gui: Arc<GuiApi>, ui: slint::Weak<AppWindow>) -> Self {
        // All errors encountered here are unrecoverable.
        // The app cannot function without seed_table and settings.
        Self {
            ui,
            gui,
            seed_table: OrderedTable::new()
                .with_persistence(FilePersistence::new(String::from(DATABASE_FILE), fs::Location::AppData))
                .expect("failed to create seed vault database"),
            search_text: String::new(),
            pending_import: None,
            archive_mode: false,
            secp: Secp256k1::signing_only(),
            security_api: Security::default(),
            model: Rc::new(VecModel::default()),
            #[cfg(not(test))]
            settings: JsonBacked::new("settings.json", fs::Location::AppData).0,
            #[cfg(test)]
            sort_mode: CardSortMode::Label,
        }
    }

    pub fn get_sort_mode(&self) -> CardSortMode {
        #[cfg(not(test))]
        return self.settings.sort_mode.clone();
        #[cfg(test)]
        return self.sort_mode;
    }

    #[cfg(not(test))]
    pub fn set_sort_mode(&mut self, mode: CardSortMode) { self.settings.guard().sort_mode = mode; }

    #[cfg(test)]
    pub fn set_sort_mode(&mut self, mode: CardSortMode) { self.sort_mode = mode; }

    pub fn is_empty(&self) -> bool { self.seed_table.is_empty() }

    pub fn ui(&self) -> AppWindow { self.ui.unwrap() }

    pub fn validate_new_label(&self, label: String) -> Result<(), VaultError> {
        SeedEditField::Label(label.clone()).validate()?;

        if let Some(_s) = self.seed_table.iter().find(|seed| seed.get_label() == &label) {
            return Err(VaultError::from(SeedDuplicateReason::Label(label)));
        }

        Ok(())
    }

    pub fn validate_new_index(&self, seed_type: SeedType) -> Result<(), VaultError> {
        // This works like a find, but avoids re-calculating the dupe_reason
        if let Some(dupe_reason) = self
            .seed_table
            .iter()
            .filter_map(|seed| seed_type.is_duplicate(&seed.seed, seed.label.clone()))
            .next()
        {
            return Err(VaultError::from(dupe_reason));
        }

        Ok(())
    }

    pub fn save_from_view(
        &mut self,
        seed_view: SeedView,
        nsec: Option<SharedString>,
        seed_entropy: Option<SharedString>,
    ) -> CallbackResult {
        let seed = match Seed::from_view(seed_view, nsec, seed_entropy) {
            Ok(s) => s,
            Err(e) => return CallbackResult::from(e),
        };

        if let Err(e) = self.save(seed) {
            return CallbackResult::from(e);
        }

        self.update_accounts();
        self.nav_accounts();
        CallbackResult::success()
    }

    pub fn save(&mut self, seed: Seed) -> Result<(), VaultError> {
        self.seed_table.separate_categories(|s| s.get_category());
        self.seed_table.push_categorized(|s| s.get_category(), seed)?;
        Ok(())
    }

    pub fn update_accounts(&mut self) {
        self.model.clear();

        let filter = if self.search_text.is_empty() {
            None
        } else {
            Some(FuzzyFilter::new(self.search_text.as_ref()))
        };

        let entries = self
            .seed_table
            .view_sorted(|a, b| Seed::compare_by(a, b, self.get_sort_mode()))
            .filter(|(_i, entry)| {
                if entry.archived != self.archive_mode {
                    return false;
                }

                match &filter {
                    Some(filter) if !filter.matches(entry.get_label().to_lowercase().as_ref()) => false,
                    _ => true,
                }
            })
            .map(|(i, entry)| SeedView::from_seed(entry).with_index(i as i32))
            .collect::<Vec<SeedView>>();

        self.model.extend(entries);
        self.ui().global::<Callbacks>().set_entries(ModelRc::from(self.model.clone()));
    }

    pub fn nav_accounts(&mut self) {
        let ui = self.ui();
        let ui_nav = ui.global::<Navigate>();

        ui_nav.invoke_return_home();

        ui_nav.invoke_accounts(
            AccountsParams::default(),
            NavigateOptions { replace: true, animate: Animate::None },
        );
    }

    pub fn move_position(&mut self, index: i32, up: bool) -> Result<(), VaultError> {
        let destination = usize::try_from(index + if up { -1 } else { 1 })?;
        let index = usize::try_from(index)?;

        // OrderedTable returns errors safely for underflows
        self.seed_table.move_position_categorized(|s| s.get_category(), index, destination)?;
        Ok(())
    }

    pub fn get_next_index(&self, seed_view_type: SeedViewType) -> u32 {
        let mut taken_indices = self
            .seed_table
            .iter()
            .filter_map(|s| match (seed_view_type, s.seed.clone()) {
                (SeedViewType::Bitcoin12, SeedType::Bitcoin12 { index })
                | (SeedViewType::Bitcoin24, SeedType::Bitcoin24 { index })
                | (SeedViewType::NostrKey, SeedType::NostrKey { index })
                | (
                    SeedViewType::PasswordGenerated,
                    SeedType::PasswordGenerated { account: _, password: _, index },
                ) => Some(index),
                (_, _) => None,
            })
            .collect::<Vec<u32>>();
        taken_indices.sort();

        // Find the first space in the sortted list of taken account indices
        // This should only happen if the user manually adds custom accounts that cause a gap in the range.
        // Otherwise, the next_index will be incremented up to the number of accounts.
        let mut next_index: u32 = 0;
        for i in taken_indices.iter() {
            if next_index != *i {
                return next_index;
            } else {
                next_index += 1;
            }
        }

        next_index
    }

    pub fn validate_edit_label(&mut self, index: i32, new_label: String) -> Result<(), VaultError> {
        let index = usize::try_from(index)?;

        let _ =
            self.seed_table.validate_edit(index, move |s| s.edit(SeedEditField::Label(new_label.clone())))?;
        Ok(())
    }

    pub fn edit_indexed(&mut self, index: i32, new_label: String, new_color: u8) -> Result<(), VaultError> {
        let index = usize::try_from(index)?;

        let _ = self.seed_table.edit(index, move |s| {
            s.edit(SeedEditField::Label(new_label.clone()))?;
            s.color = new_color;
            Ok(())
        })?;

        Ok(())
    }

    pub fn edit_password(
        &mut self,
        index: i32,
        new_label: String,
        new_account: String,
        new_password: String,
        new_color: u8,
    ) -> Result<(), VaultError> {
        let index = usize::try_from(index)?;

        let _ = self.seed_table.edit(index, move |s| {
            s.edit(SeedEditField::Label(new_label.clone()))?;
            s.edit(SeedEditField::Account(new_account.clone()))?;
            s.edit(SeedEditField::Password(new_password.clone()))?;
            s.color = new_color;
            Ok(())
        })?;

        Ok(())
    }

    pub fn set_archived(&mut self, index: i32, archived: bool) -> Result<(), VaultError> {
        let index = usize::try_from(index)?;

        let _ = self.seed_table.edit(index, move |s| {
            s.archived = archived;
            Ok(())
        })?;

        self.seed_table.separate_categories(|s| s.get_category());
        Ok(())
    }

    pub fn delete(&mut self, index: i32) -> Result<(), VaultError> {
        let index = usize::try_from(index)?;

        let _ = self.seed_table.remove(index)?;
        Ok(())
    }

    fn get_root_mnemonic(&self) -> Result<Mnemonic, VaultError> {
        let entropy = self
            .security_api
            .seed()
            .map_err(|e| VaultError::from(anyhow::anyhow!("Could not retrieve bitcoin seed: {:?}", e)))?
            .ok_or(anyhow::anyhow!("No seed or error returned, securam may be corrupt"))?;

        Ok(Mnemonic::from_entropy(entropy.bytes()).context("Could not construct mnemonic from entropy")?)
    }

    fn get_bip85_child_mnemonic(&self, words: u32, index: u32) -> Result<Mnemonic, VaultError> {
        let mnemonic = self.get_root_mnemonic()?;
        let key = mnemonic.to_seed("");
        let xpriv =
            Xpriv::new_master(Network::Bitcoin, &key).context("Could not construct xpriv from key")?;

        match words {
            12 | 24 => (),
            other => {
                return Err(VaultError::from(anyhow::anyhow!(
                    "Only 12 and 24 word child seeds are supported, not {}.",
                    other
                )));
            }
        }

        bip85_extended::to_mnemonic(&self.secp, &xpriv, words, index)
            .map_err(|e| VaultError::from(anyhow::anyhow!("Could not derive bitcoin seed: {:?}", e)))
    }

    pub fn get_entry_mnemonic(&self, index: i32) -> Result<Mnemonic, VaultError> {
        let index = usize::try_from(index)?;
        let seed = self.seed_table.get(index)?;

        let mnemonic = match &seed.seed {
            SeedType::Bitcoin12 { index: seed_index } => self.get_bip85_child_mnemonic(12, *seed_index)?,
            SeedType::Bitcoin24 { index: seed_index } => self.get_bip85_child_mnemonic(24, *seed_index)?,
            SeedType::BitcoinImport { mnemonic: ref m } => m.clone(),
            _ => return Err(VaultError::from(anyhow::anyhow!("Unable to get words for non-bitcoin item"))),
        };

        Ok(mnemonic)
    }

    fn render_bw_qr_code(&self, data: Vec<u8>) -> Image {
        slint_keyos_platform::qrcode::render(
            &data,
            slint::Color::from_rgb_u8(0, 0, 0),       // black
            slint::Color::from_rgb_u8(255, 255, 255), // white
        )
    }

    pub fn get_standard_seed_qr(&self, index: i32) -> Result<Image, VaultError> {
        let mnemonic = self.get_entry_mnemonic(index)?;
        let seed = SecuritySeed::from_mnemonic(&mnemonic);
        let data = seed.to_standard_seed_qr_data().map_err(|e| {
            VaultError::from(anyhow::anyhow!("Could not convert seed to standard SeedQR: {:?}", e))
        })?;
        Ok(self.render_bw_qr_code(data))
    }

    pub fn get_compact_seed_qr(&self, index: i32) -> Result<Image, VaultError> {
        let mnemonic = self.get_entry_mnemonic(index)?;
        let seed = SecuritySeed::from_mnemonic(&mnemonic);
        let data = seed.to_compact_seed_qr_data().map_err(|e| {
            VaultError::from(anyhow::anyhow!("Could not convert seed to compact SeedQR: {:?}", e))
        })?;
        Ok(self.render_bw_qr_code(data))
    }

    fn get_nostr_key(&self, index: i32) -> Result<nostr::Keys, VaultError> {
        let index = usize::try_from(index)?;
        let seed = self.seed_table.get(index)?;

        let nostr_key = match &seed.seed {
            SeedType::NostrKey { index: seed_index } => {
                let mnemonic = self.get_root_mnemonic()?;
                nostr::Keys::from_mnemonic_with_account(mnemonic.to_string(), None, Some(*seed_index))
                    .context("Could not derive nostr key")?
            }
            SeedType::NostrKeyImport { ref nsec } => {
                let secret =
                    nostr::SecretKey::from_bech32(nsec).context("Could not get nostr secret from nsec")?;
                nostr::Keys::new(secret)
            }
            _ => return Err(VaultError::from(anyhow::anyhow!("Unable to get nostr keys for seed"))),
        };

        Ok(nostr_key)
    }

    pub fn get_npub(&self, index: i32) -> Result<String, VaultError> {
        let key = self.get_nostr_key(index)?;
        key.public_key()
            .to_bech32()
            .map_err(|e| VaultError::from(anyhow::anyhow!("Could not build nostr npub: {:?}", e)))
    }

    pub fn get_nsec(&self, index: i32) -> Result<String, VaultError> {
        let key = self.get_nostr_key(index)?;
        key.secret_key()
            .to_bech32()
            .map_err(|e| VaultError::from(anyhow::anyhow!("Could not build nostr nsec: {:?}", e)))
    }

    pub fn get_fingerprint(&self, index: i32) -> Result<String, VaultError> {
        let mnemonic = self.get_entry_mnemonic(index)?;
        self.get_seed_fingerprint(&mnemonic)
    }

    pub fn generate_password(&self, password_index: u32, length: u32) -> Result<String, VaultError> {
        let mnemonic = self.get_root_mnemonic()?;
        let key = mnemonic.to_seed("");
        let xpriv =
            Xpriv::new_master(Network::Bitcoin, &key).context("Could not construct xpriv from key")?;

        bip85_extended::to_pwd_base85(&self.secp, &xpriv, length, password_index)
            .map_err(|e| VaultError::from(anyhow::anyhow!("Could not generate password: {:?}", e)))
    }

    pub fn get_seed_fingerprint(&self, mnemonic: &Mnemonic) -> Result<String, VaultError> {
        let key = mnemonic.to_seed("");
        let xpriv =
            Xpriv::new_master(Network::Bitcoin, &key).context("Could not construct xpriv from key")?;
        Ok(xpriv.fingerprint(&self.secp).to_string().to_uppercase())
    }

    pub fn handle_qr_input(&mut self, data: Vec<u8>) {
        if let Err(e) = self.handle_qr_data(data) {
            log::error!("Failed to handle QR data: {:?}", e);
            self.ui().global::<Navigate>().invoke_import_item_failed(NavigateOptions::default());
        }
    }

    pub fn handle_qr_data(&mut self, data: Vec<u8>) -> Result<(), VaultError> {
        let ui = self.ui();
        let ui_nav = ui.global::<Navigate>();

        if let Ok(mnemonic) = security::parse_seedqr(&data).context("Failed to parse SeedQR") {
            if !matches!(mnemonic.word_count(), 12 | 24) {
                return Err(VaultError::UnknownQrDataType);
            }
            let fingerprint = self.get_seed_fingerprint(&mnemonic)?;
            let entropy = hex::encode_upper(mnemonic.to_entropy());

            self.pending_import = Some(PendingImport::ImportedSeed { entropy, fingerprint });

            ui_nav.invoke_view_seed(NavigateOptions::default());
            return Ok(());
        };

        let text = std::str::from_utf8(data.as_slice()).context("Failed to parse as utf8")?;

        if let Ok(_secret_key) = nostr::SecretKey::from_bech32(&text) {
            self.pending_import = Some(PendingImport::NostrKey(text.to_owned()));
            ui_nav.invoke_nostr_key(NavigateOptions::default());
            return Ok(());
        }

        Err(VaultError::UnknownQrDataType)
    }

    pub fn get_seed_view_by_index(&self, index: i32) -> SeedView {
        let Ok(usize_index) = usize::try_from(index) else {
            return SeedView::default();
        };
        self.seed_table
            .get(usize_index)
            .map(|seed| SeedView::from_seed(&seed).with_index(index))
            .unwrap_or_default()
    }
}
