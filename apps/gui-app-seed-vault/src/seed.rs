// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{error::VaultError, IndexedSeedView, PasswordView, SeedView, SeedViewType},
    anyhow::Context,
    bip85_extended::bip39::Mnemonic,
    nostr::{nips::nip19, FromBech32},
    ordered_table::{SortableCard, TableEntry},
    serde::{Deserialize, Serialize},
    slint_keyos_platform::slint::SharedString,
    std::time::Duration,
};

#[derive(PartialEq, Debug, thiserror::Error)]
pub enum SeedValidationError {
    #[error("Invalid label, labels must not be empty")]
    InvalidLabelError,
    #[error("Invalid password, passwords must not be empty")]
    EmptyPasswordError,
    #[error("Invalid nsec")]
    InvalidNsecError(#[from] nip19::Error),
}

#[derive(PartialEq, Debug, thiserror::Error)]
pub enum SeedDuplicateReason {
    #[error("Duplicate label: {0:?}")]
    Label(String),
    #[error("Duplicate 12 word seed with label {0:?}")]
    Bitcoin12(String),
    #[error("Duplicate 24 word seed with label {0:?}")]
    Bitcoin24(String),
    #[error("Duplicate generated password with label {0:?}")]
    PasswordGeneratedIndex(String),
    #[error("Duplicate Nostr key with label {0:?}")]
    NostrKey(String),
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub enum SeedType {
    Bitcoin12 { index: u32 },
    Bitcoin24 { index: u32 },
    Password { account: String, password: String },
    PasswordGenerated { account: String, password: String, index: u32 },
    NostrKey { index: u32 },
    NostrKeyImport { nsec: String },
    BitcoinImport { mnemonic: Mnemonic },
}

impl Default for SeedType {
    fn default() -> Self { Self::Bitcoin12 { index: 0 } }
}

impl std::fmt::Debug for SeedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bitcoin12 { .. } => write!(f, "Bitcoin12"),
            Self::Bitcoin24 { .. } => write!(f, "Bitcoin24"),
            Self::Password { .. } => write!(f, "Password"),
            Self::PasswordGenerated { .. } => write!(f, "PasswordGenerated"),
            Self::NostrKey { .. } => write!(f, "NostrKey"),
            Self::NostrKeyImport { .. } => write!(f, "NostrKeyImport"),
            Self::BitcoinImport { .. } => write!(f, "BitcoinImport"),
        }
    }
}

fn parse_index(seed_index: Option<String>) -> Result<u32, VaultError> {
    let index = seed_index
        .ok_or_else(|| VaultError::from(anyhow::anyhow!("Unable to make indexed seed type without an index")))
        .map(|i| i.trim().parse::<u32>().unwrap_or(0))?;
    if index >= 0x80000000 {
        return Err(VaultError::from(anyhow::anyhow!(
            "Index {index} exceeds maximum BIP32 hardened index (2147483647)"
        )));
    }
    Ok(index)
}

impl SeedType {
    pub fn from_view_type(
        seed_type: SeedViewType,
        seed_index: Option<String>,
        account: Option<String>,
        password: Option<String>,
        nsec: Option<String>,
        seed_entropy: Option<String>,
    ) -> Result<Self, VaultError> {
        let account = account.unwrap_or_default();
        let password = password.unwrap_or_default();

        let new_seed_type = match seed_type {
            SeedViewType::Bitcoin12 => SeedType::Bitcoin12 { index: parse_index(seed_index)? },
            SeedViewType::Bitcoin24 => SeedType::Bitcoin24 { index: parse_index(seed_index)? },
            SeedViewType::Password => SeedType::Password { account, password },
            SeedViewType::PasswordGenerated => {
                SeedType::PasswordGenerated { account, password, index: parse_index(seed_index)? }
            }
            SeedViewType::NostrKey => SeedType::NostrKey { index: parse_index(seed_index)? },
            SeedViewType::NostrKeyImport => {
                let nsec = nsec.ok_or_else(|| {
                    VaultError::from(anyhow::anyhow!("Unable to build nostr import, no nsec"))
                })?;
                nostr::SecretKey::from_bech32(&nsec).context("Could not get nostr secret from nsec")?;
                SeedType::NostrKeyImport { nsec }
            }
            SeedViewType::BitcoinImport12 | SeedViewType::BitcoinImport24 => {
                let entropy = seed_entropy.ok_or_else(|| {
                    VaultError::from(anyhow::anyhow!("Unable to build bitcoin import, no seed words"))
                })?;
                let entropy = hex::decode(entropy).context("Could not decode entropy")?;
                let mnemonic = Mnemonic::from_entropy(entropy.as_slice())
                    .context("Could not build mnemonic from entropy")?;
                SeedType::BitcoinImport { mnemonic }
            }
        };

        // Validate here to avoid repeated parameter validation in match body
        new_seed_type.validate()?;

        Ok(new_seed_type)
    }

    fn validate(&self) -> Result<(), SeedValidationError> {
        match self {
            SeedType::Password { account: _, password } => {
                SeedEditField::Password(password.clone()).validate()?;
            }
            _ => (),
        }

        Ok(())
    }

    // Delegate seed duplication check, but require label for nice error prints
    pub fn is_duplicate(&self, other: &Self, other_label: String) -> Option<SeedDuplicateReason> {
        match (self, other) {
            (SeedType::Bitcoin12 { index: index_a }, SeedType::Bitcoin12 { index: index_b })
                if index_a == index_b =>
            {
                return Some(SeedDuplicateReason::Bitcoin12(other_label));
            }
            (SeedType::Bitcoin24 { index: index_a }, SeedType::Bitcoin24 { index: index_b })
                if index_a == index_b =>
            {
                return Some(SeedDuplicateReason::Bitcoin24(other_label));
            }
            (
                SeedType::PasswordGenerated { account: _, password: _, index: index_a },
                SeedType::PasswordGenerated { account: _, password: _, index: index_b },
            ) if index_a == index_b => {
                return Some(SeedDuplicateReason::PasswordGeneratedIndex(other_label));
            }
            //     match (account_a == account_b, password_a == password_b) {
            //         (true, true) => return Some(SeedDuplicateReason::SamePassword(account_a.clone())),
            //         (true, false) => return
            // Some(SeedDuplicateReason::DifferentPassword(account_a.clone())),         (false, _)
            // => (),         // Note: could add error/warning for password reuse
            //     }
            // }
            (SeedType::NostrKey { index: index_a }, SeedType::NostrKey { index: index_b })
                if index_a == index_b =>
            {
                return Some(SeedDuplicateReason::NostrKey(other_label));
            }
            (_, _) => (),
        }

        None
    }
}

// Always provide defaults for new values
// Requires debug to debug associated types in OrderedTable
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Seed {
    pub label: String,
    #[serde(default)]
    pub color: u8,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    date: u64,
    #[serde(default)]
    pub seed: SeedType,
}

impl TableEntry for Seed {
    type DuplicateReason = SeedDuplicateReason;
    type ValidationError = SeedValidationError;

    fn validate(&self) -> Result<(), Self::ValidationError> {
        SeedEditField::Label(self.label.clone()).validate()?;
        self.seed.validate()?;

        Ok(())
    }

    fn is_duplicate(&self, other: &Self) -> Option<Self::DuplicateReason> {
        if self.label == other.label {
            return Some(SeedDuplicateReason::Label(self.label.clone()));
        }

        self.seed.is_duplicate(&other.seed, other.label.clone())
    }
}

impl SortableCard for Seed {
    fn get_label(&self) -> &String { &self.label }

    fn get_date(&self) -> u64 { self.date }
}

#[repr(u32)]
pub enum SeedCategories {
    Active = 0,
    Archived,
}

impl Seed {
    pub fn new(seed: SeedType, label: String, color: u8, date: u64) -> Result<Self, SeedValidationError> {
        SeedEditField::Label(label.clone()).validate()?;
        seed.validate()?;

        let seed = Self { label, color, archived: false, date, seed };

        Ok(seed)
    }

    pub fn from_view(
        seed_view: SeedView,
        nsec: Option<SharedString>,
        seed_entropy: Option<SharedString>,
    ) -> Result<Self, VaultError> {
        let seed_type = SeedType::from_view_type(
            seed_view.seed_type,
            Some(seed_view.indexed_seed.index.into()),
            Some(seed_view.password.account.into()),
            Some(seed_view.password.password.into()),
            nsec.map(String::from),
            seed_entropy.map(String::from),
        )?;

        let time = get_timestamp_in_seconds();
        Ok(Self::new(seed_type, seed_view.label.clone().into(), seed_view.color as u8, time)?)
    }

    pub fn get_category(&self) -> u32 {
        (if self.archived { SeedCategories::Archived } else { SeedCategories::Active }) as u32
    }

    pub fn edit(&mut self, field: SeedEditField) -> Result<(), SeedValidationError> {
        field.validate()?;
        match (&mut self.seed, field) {
            (_, SeedEditField::Label(val)) => self.label = val,
            (SeedType::Password { ref mut account, password: _ }, SeedEditField::Account(val)) => {
                *account = val
            }
            (SeedType::Password { account: _, ref mut password }, SeedEditField::Password(val)) => {
                *password = val
            }
            (
                SeedType::PasswordGenerated { ref mut account, password: _, index: _ },
                SeedEditField::Account(val),
            ) => *account = val,
            _ => {
                log::warn!("Unsupported edit");
            }
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error, Clone)]
pub enum SeedEditField {
    #[error("label: {0:?}")]
    Label(String),
    #[error("account: {0:?}")]
    Account(String),
    #[error("password: {0:?}")]
    Password(String),
}

impl SeedEditField {
    pub fn validate(&self) -> Result<(), SeedValidationError> {
        match self {
            SeedEditField::Label(val) => {
                if val.is_empty() {
                    return Err(SeedValidationError::InvalidLabelError);
                }
            }
            SeedEditField::Password(val) => {
                if val.is_empty() {
                    return Err(SeedValidationError::EmptyPasswordError);
                }
            }
            _ => (),
        }

        Ok(())
    }
}

impl SeedView {
    pub fn from_seed(seed: &Seed) -> Self {
        let mut view = Self {
            label: SharedString::from(seed.get_label()),
            color: seed.color as i32,
            index: -1,
            ..Default::default()
        };

        let seed_type = match seed.seed {
            SeedType::Bitcoin12 { index } => {
                view.set_seed_index(index);
                SeedViewType::Bitcoin12
            }
            SeedType::Bitcoin24 { index } => {
                view.set_seed_index(index);
                SeedViewType::Bitcoin24
            }
            SeedType::Password { ref account, ref password } => {
                view.set_password(account, password);
                SeedViewType::Password
            }
            SeedType::PasswordGenerated { ref account, ref password, index } => {
                view.set_seed_index(index);
                view.set_password(account, password);
                SeedViewType::PasswordGenerated
            }
            SeedType::NostrKey { index } => {
                view.set_seed_index(index);
                SeedViewType::NostrKey
            }
            SeedType::NostrKeyImport { nsec: _ } => SeedViewType::NostrKeyImport,
            SeedType::BitcoinImport { ref mnemonic } => match mnemonic.word_count() {
                12 => SeedViewType::BitcoinImport12,
                24 => SeedViewType::BitcoinImport24,
                other => {
                    log::error!("Unsupported mnemonic length {}, assuming 24", other);
                    SeedViewType::BitcoinImport24
                }
            },
        };

        view.seed_type = seed_type;

        view
    }

    pub fn set_seed_index(&mut self, seed_index: u32) {
        self.indexed_seed = IndexedSeedView { index: seed_index.to_string().into() };
    }

    pub fn set_password(&mut self, account: &str, password: &str) {
        self.password = PasswordView { account: account.into(), password: password.into() }
    }

    pub fn with_index(mut self, index: i32) -> Self {
        self.index = index;
        self
    }
}

fn get_timestamp_in_seconds() -> u64 {
    #[cfg(not(test))]
    return std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|e| {
            log::error!("Could not get time: {:?}", e);
            Duration::ZERO
        })
        .as_secs();
    #[cfg(test)]
    return 0;
}

impl Seed {}
