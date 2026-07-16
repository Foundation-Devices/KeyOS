// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use slint_keyos_platform::{spawn_local, TaskHandle};
use {
    crate::{
        account_id::AccountId,
        bitcoin_settings::BitcoinSettings,
        fs_permissions::FileSystemPermissions,
        load::{self},
        psbt_signing::PendingPsbt,
        store::{fingerprint_for_passphrase, AccountStore, CreateMultiSigAccount, CreateSingleSigAccount},
        AccountView, AddressType, AppWindow, Callbacks, CardColor, KeychainKind, MultiSigSignerView,
        MultiSigView, Network, NetworkKind, QlStatus, SettingsApi, SingleSigView,
    },
    anyhow::{self, Context},
    ngwallet::{
        account::RemoteUpdate,
        bdk_wallet::{
            bitcoin::{bip32::Fingerprint, Address, Network as NgNetwork},
            KeychainKind as NgKeychainKind,
        },
        config::{
            AddressType as NgAddressType, MultiSigDetails, NetworkKind as NgNetworkKind, NgAccountConfig,
        },
    },
    quantum_link::{
        foundation_api::bitcoin::AccountUpdate,
        messages::{SendAccountUpdate, SendApplyPassphrase, SendPrimeFiatPreference},
    },
    slint_keyos_platform::{
        file_backed::JsonBacked,
        slint::{self, ComponentHandle, ModelRc, SharedString, ToSharedString, VecModel},
        spawn_worker, StoredValue,
    },
    std::{iter::zip, rc::Rc},
};

#[derive(Clone, Copy)]
pub struct PendingSingleSig {
    pub index: u32,
    pub network: NgNetwork,
}

pub struct AppState {
    pub ui: slint::Weak<AppWindow>,
    pub settings: JsonBacked<BitcoinSettings, FileSystemPermissions>,

    pub store: AccountStore,
    pub model: Rc<VecModel<AccountView>>,

    pub system_settings: SettingsApi,
    pub ql_status: QlStatus,

    pub pending_multisig: Option<MultiSigDetails>,
    pub pending_singlesig: Option<PendingSingleSig>,
    pub pending_psbt: PendingPsbt,
    pub pending_archived_account_id: Option<AccountId>,
    pub archive_mode: bool,
    pub pending_passphrase_task: Option<TaskHandle<()>>,
    pub pending_send_apply_passphrase: Option<TaskHandle<()>>,
    pub pending_send_fiat_preference: Option<TaskHandle<()>>,
    pub skeleton_count: usize,
    pub pending_try_passphrase: Option<TaskHandle<()>>,
}

impl AppState {
    pub fn new(ui: slint::Weak<AppWindow>) -> Self {
        Self {
            ui,
            settings: JsonBacked::new("settings.json", fs::Location::AppData).0,

            store: AccountStore::default(),
            model: Rc::new(VecModel::default()),

            system_settings: Default::default(),
            ql_status: QlStatus::new(slint_keyos_platform::worker().clone()),

            pending_multisig: None,
            pending_singlesig: None,
            pending_psbt: PendingPsbt::None,
            pending_archived_account_id: None,
            archive_mode: false,
            pending_passphrase_task: None,
            pending_send_apply_passphrase: None,
            pending_send_fiat_preference: None,
            skeleton_count: 1,
            pending_try_passphrase: None,
        }
    }

    pub fn ui(&self) -> AppWindow { self.ui.unwrap() }

    pub fn get_account_view_str(&self, account_id: &str) -> Option<(AccountId, AccountView)> {
        let account_id = match account_id.parse::<AccountId>() {
            Ok(acct) => acct,
            Err(e) => {
                log::error!("failed to parse account id {account_id} {e:?}");
                return None;
            }
        };
        let account = self.get_account_view(&account_id)?;

        Some((account_id, account))
    }

    pub fn get_account_view(&self, account_id: &AccountId) -> Option<AccountView> {
        let config = self.store.get_account_config(account_id)?;
        let view = convert_account(account_id, &config);
        Some(view)
    }

    pub fn refresh_slint_accounts(&self) {
        self.model.clear();
        let accounts = self.store.active_accounts().filter_map(|(id, config)| {
            if config.archived != self.archive_mode {
                return None;
            }

            Some(convert_account(id, &*config))
        });
        self.model.extend(accounts);

        if !self.archive_mode {
            for _ in 0..self.skeleton_count {
                self.model.push(AccountView { skeleton: true, ..Default::default() });
            }
        }

        let ui = self.ui();
        let cb = ui.global::<Callbacks>();
        cb.set_accounts(ModelRc::from(self.model.clone()));
    }

    pub fn get_account_addresses(
        &mut self,
        account_id: AccountId,
        keychain_kind: NgKeychainKind,
        address_type: NgAddressType,
        offset: Option<u32>,
        count: usize,
    ) -> anyhow::Result<Vec<String>> {
        let account = self.store.get_account_or_fail(&account_id)?;
        let bdk_wallet = zip(
            account
                .wallets
                .read()
                .unwrap()
                .iter(),
            account
                .config
                .read()
                .unwrap()
                .descriptors
                .iter()
            )
            // Multisigs only have one wallet, which should be selected
            .find(|(w, desc)| {
                let wallet_address_type = desc.export_addr_hint.unwrap_or(w.address_type);
                wallet_address_type == address_type || account_id.is_multi()
            })
            .map(|(w, _desc)| w)
            .with_context(|| format!("No wallet in account with address type {:?}", address_type))?
            .bdk_wallet
            .clone();

        let bdk_wallet = bdk_wallet.lock().unwrap();
        let count = std::cmp::min(count, 50);
        let start_index = offset.unwrap_or(0);

        let addresses: Vec<String> = bdk_wallet
            .unbounded_spk_iter(keychain_kind)
            .skip(start_index as usize)
            .take(count)
            .filter_map(|(i, spk)| match Address::from_script(&spk, bdk_wallet.network()) {
                Ok(a) => Some(a.to_string()),
                Err(e) => {
                    log::error!("Skipped address {}, must have address form: {}", i, e);
                    None
                }
            })
            .collect();

        Ok(addresses)
    }

    pub fn set_pending_multisig(&mut self, multisig: MultiSigDetails) -> anyhow::Result<()> {
        self.store.validate_multisig_account(&multisig, None, None)?;
        self.pending_multisig = Some(multisig);
        Ok(())
    }
}

// ASYNC FUNCTIONS
//
// if a function takes a while, and we don't want to block main thread, it should be async
// it should operate on StoredValue<AppState>, and not just AppState with a &self parameter
//
// this will allow the future generated by these functions to have a 'static lifetime
// instead of the lifetime of the &self reference
impl AppState {
    pub async fn create_singlesig_account(
        state: StoredValue<Self>,
        create: CreateSingleSigAccount,
    ) -> anyhow::Result<AccountId> {
        state.borrow().store.validate_singlesig_account(&create)?;

        let (passphrase, device_serial, master_key) = {
            let state = state.borrow();
            (
                state.store.get_passphrase().clone(),
                state.store.device_serial.clone(),
                state.store.load_master_key(create.network)?,
            )
        };

        let (account_id, account) = spawn_worker({
            let fs = crate::FileSystem::default();
            async move { load::build_singlesig_account(&fs, master_key, device_serial, passphrase, create) }
        })
        .await?;

        {
            let mut state = state.borrow_mut();
            state.store.insert_account(account_id.clone(), account);
            state.refresh_slint_accounts();
        }

        Self::publish_account_config(state, account_id.clone());

        Ok(account_id)
    }

    pub async fn create_multisig_account(
        state: StoredValue<Self>,
        create: CreateMultiSigAccount,
    ) -> anyhow::Result<AccountId> {
        state.borrow().store.validate_multisig_account(
            &create.multisig,
            Some(create.network),
            Some(&create.label),
        )?;

        let (device_serial, master_key, secp) = {
            let state = state.borrow();
            (
                state.store.device_serial.clone(),
                state.store.load_master_key(create.network)?,
                state.store.secp.clone(),
            )
        };

        let (account_id, account) = spawn_worker({
            let fs = crate::FileSystem::default();
            async move { load::build_multisig_account(&fs, &*secp, master_key, device_serial, create) }
        })
        .await?;

        {
            let mut state = state.borrow_mut();
            state.store.insert_account(account_id.clone(), account);
            state.refresh_slint_accounts();
        }

        Self::publish_account_config(state, account_id.clone());

        Ok(account_id)
    }

    /// SE read + fingerprint application. Called inside spawned tasks; never call
    /// directly from sync code. Stores handle in `pending_passphrase_task` via the
    /// callers (`apply_passphrase`, `create_initial_account`).
    pub async fn apply_passphrase_inner(state: StoredValue<Self>, passphrase: String) {
        let fingerprint = spawn_worker({
            let passphrase = passphrase.clone();
            async move { fingerprint_for_passphrase(&passphrase) }
        })
        .await
        .inspect_err(|e| log::error!("failed to compute fingerprint for passphrase: {e:?}"))
        .unwrap_or_default();

        let mut s = state.borrow_mut();
        s.store.apply_fingerprint(passphrase, fingerprint);
        if s.store.active_accounts().next().is_some() {
            s.skeleton_count = 0;
        }
        s.refresh_slint_accounts();
        if s.skeleton_count == 0 {
            s.ui().global::<Callbacks>().set_accounts_loading(false);
        }
    }

    pub fn apply_passphrase(state: StoredValue<Self>, passphrase: String) {
        {
            let mut s = state.borrow_mut();
            let ui = s.ui();
            let cb = ui.global::<Callbacks>();
            cb.set_accounts_loading(true);
            // Skeleton only belongs in the non-archive account list.
            if !s.archive_mode {
                s.skeleton_count = 1;
                s.model.clear();
                s.model.push(AccountView { skeleton: true, ..Default::default() });
                cb.set_accounts(ModelRc::from(s.model.clone()));
            }
        }

        let handle = spawn_local(async move {
            Self::apply_passphrase_inner(state, passphrase).await;
            Self::publish_accounts_and_passphrase(state);
        });
        state.borrow_mut().pending_passphrase_task = Some(handle);
    }

    pub fn publish_accounts_and_passphrase(state: StoredValue<Self>) {
        let (fingerprint, ql_status) = {
            let state = state.borrow();
            (state.store.fingerprint, state.ql_status.clone())
        };
        let handle = spawn_local(async move {
            let fingerprint_str = fingerprint.to_string();
            let message = SendApplyPassphrase {
                fingerprint: if fingerprint == Fingerprint::default() { None } else { Some(fingerprint_str) },
            };

            ql_status
                .send_ql_archive_retry(message, |e| {
                    log::error!("faled to send apply passphrase {e:?}, retrying...");
                })
                .await;

            log::info!("successfully sent apply passphrase");

            Self::publish_accounts(state);
        });

        state.borrow_mut().pending_send_apply_passphrase = Some(handle);
    }

    pub fn publish_fiat_preference(state: StoredValue<Self>) {
        let (currency_code, ql_status) = {
            let state = state.borrow();
            (state.settings.show_fiat_value.code().to_string(), state.ql_status.clone())
        };
        let handle = spawn_local(async move {
            ql_status
                .send_ql_archive_retry(SendPrimeFiatPreference { currency_code }, |e| {
                    log::warn!("failed to publish fiat preference {e:?}")
                })
                .await;
        });
        // Drop any in-flight earlier send so a stale retry can't overwrite a newer one.
        state.borrow_mut().pending_send_fiat_preference = Some(handle);
    }

    /// Switches between Default and Passphrase views locally without notifying Envoy.
    /// This is used for the Default/Passphrase toggle in the UI.
    /// Unlike `apply_passphrase`, this does NOT send any message to Envoy.
    pub fn switch_view_locally(state: StoredValue<Self>, passphrase: String) {
        let mut state = state.borrow_mut();
        state.skeleton_count = 0;
        state.store.switch_fingerprint(passphrase);
        state.refresh_slint_accounts();
    }

    pub async fn load_active_accounts(state: StoredValue<Self>) -> anyhow::Result<()> {
        {
            let mut state = state.borrow_mut();
            state.skeleton_count = 1;
            state.refresh_slint_accounts();
            let ui = state.ui();
            ui.global::<Callbacks>().set_accounts_loading(true);
        }

        let (tx, rx) = async_channel::bounded(1);
        let _worker = spawn_worker(async move {
            load::load_account_configs(tx)
                .await
                .inspect_err(|e| log::error!("failed to load account configs {e:?}"))
                .ok();
        });

        while let Ok((id, account, storage)) = rx.recv().await {
            let mut s = state.borrow_mut();
            s.store.insert_account_config(id, account, storage);
            s.skeleton_count = 0;
            s.refresh_slint_accounts();
        }

        // create initial account, if needed
        let create_result = if state.borrow().store.num_single_accounts(None) == 0
            && !state.borrow().store.has_passphrase()
        {
            let start = std::time::Instant::now();
            let r = AppState::create_singlesig_account(
                state,
                CreateSingleSigAccount {
                    label: String::from("Passport Prime"),
                    network: ngwallet::bdk_wallet::bitcoin::Network::Bitcoin,
                    index: 0,
                },
            )
            .await
            .context("failed to create single sig account");
            if r.is_ok() {
                log::info!("created account in {}ms", start.elapsed().as_millis());
            }
            Some(r)
        } else {
            None
        };

        // Always clear loading state, even when account creation fails.
        {
            let mut s = state.borrow_mut();
            if s.skeleton_count > 0 {
                s.skeleton_count = 0;
                s.refresh_slint_accounts();
            }
            let ui = s.ui();
            ui.global::<Callbacks>().set_accounts_loading(false);
        }

        create_result.transpose()?;
        Ok(())
    }

    pub fn publish_accounts(state: StoredValue<AppState>) {
        let account_ids =
            state.borrow().store.active_accounts().map(|(id, _)| id.clone()).collect::<Vec<_>>();

        for id in account_ids {
            Self::publish_account_config(state, id);
        }
    }

    // todo: this should wait until we've established a quantum link connection instead of retrying
    // indefinitely.
    pub fn publish_account_config(state: StoredValue<AppState>, id: AccountId) {
        let Some(config) = state.borrow().store.get_account_config(&id).map(|c| c.clone()) else {
            return;
        };

        let handle = spawn_local({
            let id = id.clone();
            let bt_state = state.borrow().ql_status.clone();
            let update = RemoteUpdate::new(Some(config), vec![]).serialize();
            let message = SendAccountUpdate { account_id: id.to_string(), update };
            async move {
                bt_state
                    .send_ql_archive_retry(message, |e| {
                        log::debug!("faled to send account update {e:?}, retrying...");
                    })
                    .await;
                log::info!("successfully sent account update for account {id}");
            }
        });

        let mut state = state.borrow_mut();
        // clean up completed tasks
        state.store.publish_tasks.retain(|_, task| !task.is_finished());
        // this will cancel any outstanding backup tasks
        // by dropping the task handle
        // guarantees we only are sending the latest account config
        if let Some(_) = state.store.publish_tasks.insert(id, handle) {
            log::debug!("cancelled outstanding backup task");
        }
    }

    pub fn update_account_config<R, F>(state: StoredValue<AppState>, id: AccountId, f: F) -> Option<R>
    where
        F: FnOnce(&mut NgAccountConfig) -> R,
    {
        let result = {
            let mut state = state.borrow_mut();
            let Some(mut account) = state.store.get_account_config_mut(&id) else {
                return None;
            };
            f(&mut *account)
        };

        state.borrow().refresh_slint_accounts();
        Self::publish_account_config(state, id);
        Some(result)
    }

    pub async fn process_account_update(
        state: StoredValue<AppState>,
        AccountUpdate { account_id, update }: AccountUpdate,
    ) -> anyhow::Result<()> {
        log::info!("Received quantum link account update {account_id}");

        let account_id = account_id.parse::<AccountId>().context("invalid account id")?;

        if state.borrow().store.publish_tasks.get(&account_id).is_some_and(|t| !t.is_finished()) {
            log::info!("detected update collision. not applying incoming account update for {account_id}");
            return Ok(());
        }

        let load_account = state.borrow().store.load_account(account_id);
        let (id, account) = load_account.await.context("load acct")?;
        if let Err(e) = account.update(update) {
            log::error!("failed to apply account update {id} {e:?}");
        }

        let mut state = state.borrow_mut();
        state.store.insert_account(id, account);
        state.refresh_slint_accounts();
        Ok(())
    }

    pub fn set_archive_mode(state: StoredValue<AppState>, mode: bool) {
        let mut state = state.borrow_mut();
        state.archive_mode = mode;
        state.refresh_slint_accounts();
    }

    pub fn delete_account(state: StoredValue<AppState>, id: AccountId) {
        let mut state = state.borrow_mut();
        state.store.delete_account(id);
        state.refresh_slint_accounts();
    }
}

fn convert_account(account_id: &AccountId, config: &NgAccountConfig) -> AccountView {
    // Single-sig: derive from index so it cycles like Core/Envoy.
    let color: CardColor = match config.archived {
        true => CardColor::DarkGrey,
        false if config.multisig.is_none() => AccountColor::for_account_index(config.index).into(),
        false => AccountColor::from_hex(&config.color).into(),
    };

    match &config.multisig {
        Some(multisig) => AccountView {
            id: account_id.to_shared_string(),
            name: config.name.to_shared_string(),
            is_multisig: true,
            network: config.network.into(),
            color,
            archived: config.archived,
            single: SingleSigView::default(),
            multi: MultiSigView::from(multisig),
            skeleton: false,
        },
        None => {
            let fingerprint = hex::encode_upper(account_id.fingerprint().copied().unwrap_or_default());
            AccountView {
                id: account_id.to_shared_string(),
                name: config.name.to_shared_string(),
                is_multisig: false,
                network: config.network.into(),
                color,
                archived: config.archived,
                single: SingleSigView {
                    account_number: config.index as i32,
                    fingerprint: SharedString::from(fingerprint),
                    // TODO: get account xpub, should probably also be a function
                    // or stored in NgAccountConfig
                    ..Default::default()
                },
                multi: MultiSigView::default(),
                skeleton: false,
            }
        }
    }
}

impl From<&MultiSigDetails> for MultiSigView {
    fn from(multisig: &MultiSigDetails) -> Self {
        let signers = multisig
            .get_signers()
            .iter()
            .map(|s| MultiSigSignerView {
                fingerprint: hex::encode_upper(s.get_fingerprint().to_bytes()).to_shared_string(),
                derivation_path: s.get_derivation_inner().to_shared_string(),
                public_key: s.get_pubkey_str().into(),
            })
            .collect::<Vec<MultiSigSignerView>>();

        MultiSigView {
            policy_threshold: multisig.policy_threshold as i32,
            policy_total_keys: multisig.policy_total_keys as i32,
            network_kind: multisig.network_kind.into(),
            addresses: multisig.format.into(),
            signers: ModelRc::from(Rc::new(VecModel::from(signers))),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum AccountColor {
    DarkGrey,
    Purple,
    Green,
    Pine,
    #[default]
    LightCopper,
    DarkCopper,
    Teal,
    Blue,
}

impl AccountColor {
    pub fn to_hex(&self) -> &'static str {
        match self {
            AccountColor::DarkGrey => "#747374",    // neutral-700
            AccountColor::Purple => "#9e57ff",      // purple-500
            AccountColor::Green => "#2e9483",       // green-500
            AccountColor::Pine => "#007a7a",        // pine-500
            AccountColor::LightCopper => "#d68b6e", // light-copper-500
            AccountColor::DarkCopper => "#bf755f",  // dark-copper-500
            AccountColor::Teal => "#00a5b2",        // teal-500
            AccountColor::Blue => "#009db9",        // blue-500
        }
    }

    pub fn from_hex(hex: &str) -> Self {
        match hex {
            "#747374" => AccountColor::DarkGrey,
            "#9e57ff" => AccountColor::Purple,
            "#2e9483" => AccountColor::Green,
            "#007a7a" => AccountColor::Pine,
            "#d68b6e" => AccountColor::LightCopper,
            "#bf755f" => AccountColor::DarkCopper,
            "#00a5b2" => AccountColor::Teal,
            "#009db9" => AccountColor::Blue,
            _ => AccountColor::LightCopper,
        }
    }

    // Match Core/Envoy's acct_num % 6 cycling. Single-sig only; multisig keeps stored color.
    pub fn for_account_index(index: u32) -> Self {
        match index % 6 {
            0 => AccountColor::DarkCopper,
            1 => AccountColor::Blue,
            2 => AccountColor::Pine,
            3 => AccountColor::LightCopper,
            4 => AccountColor::Teal,
            _ => AccountColor::Green,
        }
    }
}

impl From<AccountColor> for CardColor {
    fn from(color: AccountColor) -> Self {
        match color {
            AccountColor::DarkGrey => CardColor::DarkGrey,
            AccountColor::Purple => CardColor::Purple,
            AccountColor::Green => CardColor::Green,
            AccountColor::Pine => CardColor::Pine,
            AccountColor::LightCopper => CardColor::LightCopper,
            AccountColor::DarkCopper => CardColor::DarkCopper,
            AccountColor::Teal => CardColor::Teal,
            AccountColor::Blue => CardColor::Blue,
        }
    }
}

impl From<CardColor> for AccountColor {
    fn from(color: CardColor) -> Self {
        match color {
            CardColor::DarkGrey => AccountColor::DarkGrey,
            CardColor::Purple => AccountColor::Purple,
            CardColor::Green => AccountColor::Green,
            CardColor::Pine => AccountColor::Pine,
            CardColor::LightCopper => AccountColor::LightCopper,
            CardColor::DarkCopper => AccountColor::DarkCopper,
            CardColor::Teal => AccountColor::Teal,
            CardColor::Blue => AccountColor::Blue,
            CardColor::Orange => AccountColor::Blue,
            CardColor::Red => AccountColor::Blue,
        }
    }
}

impl From<NgAddressType> for AddressType {
    fn from(address_type: NgAddressType) -> Self {
        match address_type {
            NgAddressType::P2pkh => AddressType::P2pkh,
            NgAddressType::P2sh => AddressType::P2sh,
            NgAddressType::P2wpkh => AddressType::P2wpkh,
            NgAddressType::P2wsh => AddressType::P2wsh,
            NgAddressType::P2tr => AddressType::P2tr,
            NgAddressType::P2ShWpkh => AddressType::P2ShWpkh,
            NgAddressType::P2ShWsh => AddressType::P2ShWsh,
            other => todo!("Attempted converting to unknown address type: {:?}", other),
        }
    }
}

impl Into<NgAddressType> for AddressType {
    fn into(self) -> NgAddressType {
        match self {
            AddressType::P2pkh => NgAddressType::P2pkh,
            AddressType::P2sh => NgAddressType::P2sh,
            AddressType::P2wpkh => NgAddressType::P2wpkh,
            AddressType::P2wsh => NgAddressType::P2wsh,
            AddressType::P2tr => NgAddressType::P2tr,
            AddressType::P2ShWpkh => NgAddressType::P2ShWpkh,
            AddressType::P2ShWsh => NgAddressType::P2ShWsh,
        }
    }
}

impl From<NgKeychainKind> for KeychainKind {
    fn from(keychain_kind: NgKeychainKind) -> Self {
        match keychain_kind {
            NgKeychainKind::External => KeychainKind::External,
            NgKeychainKind::Internal => KeychainKind::Internal,
        }
    }
}

impl Into<NgKeychainKind> for KeychainKind {
    fn into(self) -> NgKeychainKind {
        match self {
            KeychainKind::External => NgKeychainKind::External,
            KeychainKind::Internal => NgKeychainKind::Internal,
        }
    }
}

impl From<NgNetwork> for Network {
    fn from(network: NgNetwork) -> Self {
        match network {
            NgNetwork::Bitcoin => Network::Bitcoin,
            NgNetwork::Testnet4 => Network::Testnet4,
            _ => Network::Testnet4,
        }
    }
}

impl Into<NgNetwork> for Network {
    fn into(self) -> NgNetwork {
        match self {
            Network::Bitcoin => NgNetwork::Bitcoin,
            Network::Testnet4 => NgNetwork::Testnet4,
        }
    }
}

impl From<NgNetworkKind> for NetworkKind {
    fn from(network_kind: NgNetworkKind) -> Self {
        match network_kind {
            NgNetworkKind::Main => NetworkKind::Main,
            NgNetworkKind::Test => NetworkKind::Test,
        }
    }
}

impl Into<NgNetworkKind> for NetworkKind {
    fn into(self) -> NgNetworkKind {
        match self {
            NetworkKind::Main => NgNetworkKind::Main,
            NetworkKind::Test => NgNetworkKind::Test,
        }
    }
}
