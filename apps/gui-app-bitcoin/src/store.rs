// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, RwLockReadGuard, RwLockWriteGuard},
};

use anyhow::{bail, Context};
use ngwallet::{
    account::NgAccount,
    bdk_wallet::bitcoin::{
        bip32::{ChildNumber, DerivationPath, Fingerprint, Xpriv, Xpub},
        secp256k1::{All, Secp256k1, Signing},
        Network as NgNetwork,
    },
    bip39::MasterKey,
    config::{MultiSigDetails, NetworkKind, NgAccountConfig},
    store::MetaStorage,
};
use slint_keyos_platform::{
    futures_lite::{future::Boxed, FutureExt},
    TaskHandle,
};

use crate::{
    account_id::AccountId,
    load::{delete_account_files, load_account, KeyOsWalletPersister},
    log_ms,
    sensitive_xpriv::SensitiveXpriv,
    state::AccountColor,
    tr, TrId,
};

pub struct AccountStore {
    pub secp: Arc<Secp256k1<All>>,
    pub security: crate::Security,
    pub fs: crate::FileSystem,
    pub fingerprint: Fingerprint,
    // Passphrase is private, so you have to use the setter that
    // updates the fingerprint and clears accounts.
    // Empty string is equivalent to None, just much easier to process.
    passphrase: String,
    pub device_serial: String,
    pub publish_tasks: BTreeMap<AccountId, TaskHandle<()>>,
    accounts: BTreeMap<AccountId, Account>,

    // Fingerprints cached at passphrase-application time so that local view
    // switches (Default <-> Passphrase) never need to re-read from the SE
    pub default_fingerprint: Fingerprint,
    passphrase_fingerprint: Option<Fingerprint>,
}

#[derive(Debug)]
pub enum Account {
    Config { config: NgAccountConfig, storage: Arc<dyn MetaStorage> },
    Full(NgAccount<KeyOsWalletPersister>),
}

pub(crate) const ACCOUNT_SOURCE_TAG: &str = "keyos.account.source";
pub(crate) const CASA_ACCOUNT_SOURCE: &str = "casa";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AccountSource {
    #[default]
    Generic,
    Casa,
}

fn is_casa_account_derivation(derivation: &DerivationPath, network: NetworkKind) -> bool {
    let [purpose, coin_type, account] = derivation.as_ref() else { return false };
    let expected_coin_type = match network {
        NetworkKind::Main => 0,
        NetworkKind::Test => 1,
    };

    // Casa exports m for legacy vaults and m/45' for new vaults, then returns
    // the account-level signer used by the descriptor. Legacy accounts use
    // m/49/coin/account; new accounts use m/45'/coin/account.
    matches!(
        (purpose, coin_type, account),
        (
            ChildNumber::Normal { index: 49 },
            ChildNumber::Normal { index },
            ChildNumber::Normal { .. },
        ) | (
            ChildNumber::Hardened { index: 45 },
            ChildNumber::Normal { index },
            ChildNumber::Normal { .. },
        ) if *index == expected_coin_type
    )
}

fn validate_local_multisig_signer<C: Signing>(
    secp: &Secp256k1<C>,
    master_key: &MasterKey,
    multisig: &MultiSigDetails,
) -> anyhow::Result<()> {
    let signer = multisig
        .get_signers()
        .iter()
        .find(|signer| signer.get_fingerprint() == master_key.fingerprint)
        .context("Casa multisig does not contain the active Master Key")?;
    let derivation = signer.get_derivation().context("invalid Casa signer derivation")?;
    if !is_casa_account_derivation(&derivation, multisig.network_kind) {
        bail!("Casa signer does not use a supported account derivation");
    }
    let declared_xpub = signer.get_pubkey().context("invalid Casa signer xpub")?;
    let root_xpriv = SensitiveXpriv(
        Xpriv::new_master(multisig.network_kind, &master_key.key.0)
            .context("derive Casa validation root key")?,
    );
    let signer_xpriv =
        SensitiveXpriv(root_xpriv.0.derive_priv(secp, &derivation).context("derive Casa signer key")?);
    let expected_xpub = Xpub::from_priv(secp, &signer_xpriv.0);
    if declared_xpub.public_key != expected_xpub.public_key
        || declared_xpub.chain_code != expected_xpub.chain_code
    {
        bail!("Casa signer xpub does not match the active Master Key");
    }
    Ok(())
}

impl Account {
    pub fn config(&self) -> ConfigBorrow<'_> {
        match self {
            Account::Config { config, .. } => ConfigBorrow::Ref(config),
            Account::Full(account) => ConfigBorrow::Guard(account.config.read().unwrap()),
        }
    }

    pub fn config_mut(&mut self) -> ConfigBorrowMut<'_> {
        match self {
            Account::Config { config, storage } => ConfigBorrowMut::Ref { config, storage },
            Account::Full(account) => ConfigBorrowMut::Guard {
                config: account.config.write().unwrap(),
                storage: &account.meta_storage,
            },
        }
    }

    pub fn unload(&mut self) {
        if let Account::Full(account) = self {
            let config = account.config.read().unwrap().clone();
            let storage = account.meta_storage.clone();
            *self = Account::Config { config, storage };
        }
    }

    fn source(&self) -> AccountSource {
        let storage = match self {
            Account::Config { storage, .. } => storage,
            Account::Full(account) => &account.meta_storage,
        };
        match storage.get_tag(ACCOUNT_SOURCE_TAG) {
            Ok(Some(value)) if value == CASA_ACCOUNT_SOURCE => AccountSource::Casa,
            Ok(_) => AccountSource::Generic,
            Err(error) => {
                log::error!("failed to read account source: {error:?}");
                AccountSource::Generic
            }
        }
    }
}

pub enum ConfigBorrow<'a> {
    Ref(&'a NgAccountConfig),
    Guard(RwLockReadGuard<'a, NgAccountConfig>),
}

impl<'a> std::ops::Deref for ConfigBorrow<'a> {
    type Target = NgAccountConfig;

    fn deref(&self) -> &Self::Target {
        match self {
            ConfigBorrow::Ref(config) => config,
            ConfigBorrow::Guard(guard) => &*guard,
        }
    }
}

pub enum ConfigBorrowMut<'a> {
    Ref { config: &'a mut NgAccountConfig, storage: &'a Arc<dyn MetaStorage> },
    Guard { config: RwLockWriteGuard<'a, NgAccountConfig>, storage: &'a Arc<dyn MetaStorage> },
}

impl<'a> std::ops::Deref for ConfigBorrowMut<'a> {
    type Target = NgAccountConfig;

    fn deref(&self) -> &Self::Target {
        match self {
            ConfigBorrowMut::Ref { config, .. } => config,
            ConfigBorrowMut::Guard { config, .. } => &*config,
        }
    }
}

impl<'a> std::ops::DerefMut for ConfigBorrowMut<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            ConfigBorrowMut::Ref { config, .. } => config,
            ConfigBorrowMut::Guard { config, .. } => &mut *config,
        }
    }
}

impl<'a> Drop for ConfigBorrowMut<'a> {
    fn drop(&mut self) {
        if let Err(e) = match self {
            ConfigBorrowMut::Ref { config, storage } => storage.set_config(&config.serialize()),
            ConfigBorrowMut::Guard { config, storage } => storage.set_config(&config.serialize()),
        } {
            log::error!("failed to persist account config {e:?}");
        }
    }
}

pub struct CreateSingleSigAccount {
    pub label: String,
    pub network: NgNetwork,

    pub index: u32,
}

pub struct CreateMultiSigAccount {
    pub label: String,
    pub color: AccountColor,
    pub network: NgNetwork,

    pub multisig: MultiSigDetails,
    pub source: AccountSource,
}

impl Default for AccountStore {
    fn default() -> Self {
        let secp = Secp256k1::new();
        let security = crate::Security::default();
        let master_key = load_master_key(&secp, &security, "", NgNetwork::Bitcoin).inspect_err(|e| {
            log::error!("failed to load master key {e:?}");
        });
        let fingerprint = master_key.map(|m| m.fingerprint).unwrap_or_default();
        let device_serial = security.device_id().map(|id| id.to_string()).unwrap_or_default();
        Self {
            secp: Arc::new(secp),
            security,
            fingerprint,
            device_serial,
            fs: Default::default(),
            passphrase: Default::default(),
            accounts: Default::default(),
            publish_tasks: Default::default(),
            default_fingerprint: fingerprint,
            passphrase_fingerprint: None,
        }
    }
}

impl AccountStore {
    pub const MAX_BIP32_INDEX: u32 = 0x7fff_ffff;

    pub fn insert_account_config(
        &mut self,
        id: AccountId,
        config: NgAccountConfig,
        storage: Arc<dyn MetaStorage>,
    ) {
        let _ = self.accounts.insert(id, Account::Config { config, storage });
    }

    pub fn insert_account(&mut self, id: AccountId, account: NgAccount<KeyOsWalletPersister>) {
        let _ = self.accounts.insert(id, Account::Full(account));
    }

    pub fn get_account_config(&self, id: &AccountId) -> Option<ConfigBorrow<'_>> {
        self.accounts.get(id).map(|account| account.config())
    }

    pub fn account_source(&self, id: &AccountId) -> AccountSource {
        self.accounts.get(id).map(Account::source).unwrap_or_default()
    }

    pub fn is_casa_account(&self, id: &AccountId) -> bool { self.account_source(id) == AccountSource::Casa }

    pub fn get_account_config_mut(&mut self, id: &AccountId) -> Option<ConfigBorrowMut<'_>> {
        self.accounts.get_mut(id).map(|account| account.config_mut())
    }

    pub fn get_passphrase(&self) -> &String { &self.passphrase }

    pub fn has_passphrase(&self) -> bool { !self.passphrase.is_empty() }

    pub fn num_single_accounts(&self, fp: Option<Fingerprint>) -> usize {
        let fp = fp.unwrap_or(self.fingerprint);
        self.accounts
            .iter()
            .filter(|(id, _a)| match id {
                AccountId::Single { fingerprint, .. } => fingerprint == &fp,
                AccountId::Multi { .. } => false,
            })
            .count()
    }

    // load the account in the background
    pub fn load_account(
        &self,
        id: AccountId,
    ) -> Boxed<anyhow::Result<(AccountId, NgAccount<KeyOsWalletPersister>)>> {
        let Some(account) = self.accounts.get(&id) else {
            return async move { Err(anyhow::anyhow!("account {id} not found")) }.boxed();
        };

        let (config, storage) = match account {
            Account::Config { config, storage } => (config.clone(), storage.clone()),
            Account::Full(account) => {
                log::info!("account already loaded");
                let account = account.clone();
                return async { Ok((id, account)) }.boxed();
            }
        };

        let descriptors =
            load_master_key(&self.secp, &self.security, self.passphrase.as_str(), config.network).and_then(
                |master_key| match &config.multisig {
                    Some(config) => config.get_descriptors(&self.secp, Some(&master_key)),
                    None => ngwallet::bip39::get_descriptors(&master_key.key.0, config.network, config.index),
                },
            );

        async move {
            let fs = crate::FileSystem::default();
            let descriptors = descriptors?;
            let account = load_account(&fs, &id, config, storage, descriptors)?;
            Ok((id, account))
        }
        .boxed()
    }

    pub fn get_account(
        &mut self,
        id: &AccountId,
    ) -> anyhow::Result<Option<&NgAccount<KeyOsWalletPersister>>> {
        let Some(account) = self.accounts.get_mut(&id) else {
            log::info!("account not found");
            return Ok(None);
        };

        let (config, storage) = match account {
            Account::Config { config, storage } => (config.clone(), storage.clone()),
            Account::Full(account) => {
                log::info!("account already loaded");
                return Ok(Some(account));
            }
        };

        log::info!("loading full account {id}");
        let master_key =
            load_master_key(&self.secp, &self.security, self.passphrase.as_str(), config.network)?;

        let descriptors = log_ms("get descriptors", || match &config.multisig {
            Some(config) => config.get_descriptors(&self.secp, Some(&master_key)),
            None => ngwallet::bip39::get_descriptors(&master_key.key.0, config.network, config.index),
        })
        .context("Failed to get descriptors")?;

        let full_account = load_account(&self.fs, id, config, storage, descriptors)?;

        *account = Account::Full(full_account);

        match account {
            Account::Full(ng_account) => Ok(Some(ng_account)),
            Account::Config { .. } => {
                unreachable!()
            }
        }
    }

    pub fn get_account_or_fail(
        &mut self,
        id: &AccountId,
    ) -> anyhow::Result<&NgAccount<KeyOsWalletPersister>> {
        log_ms("get_account_or_fail", || {
            self.get_account(id)?.ok_or_else(|| anyhow::anyhow!("account not found {id}"))
        })
    }

    pub fn validate_singlesig_account(&self, create: &CreateSingleSigAccount) -> anyhow::Result<()> {
        let invalid_label = self.validate_label(&create.label);
        let duplicate_index = self.validate_index(create.index, create.network);

        let account_id = AccountId::new_single(self.fingerprint, create.network, create.index);
        let duplicate_account_id = self.accounts.contains_key(&account_id);

        if invalid_label.is_some() || duplicate_index.is_some() || duplicate_account_id {
            bail!(
                "Invalid singlesig: invalid_label? {:?}, duplicate_index? {:?}, duplicate_account_id? {}",
                invalid_label,
                duplicate_index,
                duplicate_account_id
            );
        }

        Ok(())
    }

    pub fn validate_multisig_account(
        &self,
        multisig: &MultiSigDetails,
        network: Option<NgNetwork>,
        label: Option<&str>,
    ) -> anyhow::Result<()> {
        let signers = multisig.get_signers();
        let signers_set: HashSet<Fingerprint> = signers.iter().map(|s| s.get_fingerprint()).collect();

        // This check ensures that multisigs that contain the current fingerprint
        // and a passphrased fingerprint won't allow duplicates of the passphrased fingerprint
        let repeated_fingerprint = signers_set.len() != signers.len();

        // Ensure the current fingerprint owns exaclty one signer,
        // otherwise this multisig is irrelevant
        let not_user_multisig = !signers_set.contains(&self.fingerprint);

        let invalid_label = label.map_or(None, |label| self.validate_label(label));

        let duplicate_account_id = network.map_or(false, |network| {
            let account_id = AccountId::new_multi(&multisig, network);
            self.accounts.contains_key(&account_id)
        });

        let invalid_network = network.map_or(false, |network| {
            if multisig.network_kind == NetworkKind::Main {
                network != NgNetwork::Bitcoin
            } else {
                network == NgNetwork::Bitcoin
            }
        });

        if repeated_fingerprint
            || not_user_multisig
            || invalid_label.is_some()
            || duplicate_account_id
            || invalid_network
        {
            bail!("Invalid multisig: repeated_fingerprint? {}, not_user_multisig? {}, invalid_label? {:?}, duplicate_account_id? {}, invalid_network? {}", repeated_fingerprint, not_user_multisig, invalid_label, duplicate_account_id, invalid_network);
        } else {
            Ok(())
        }
    }

    pub fn validate_casa_multisig_account(
        &self,
        master_key: &MasterKey,
        multisig: &MultiSigDetails,
    ) -> anyhow::Result<()> {
        validate_local_multisig_signer(&self.secp, master_key, multisig)
    }

    pub fn validate_label(&self, label: &str) -> Option<String> {
        if self.accounts.values().any(|a| a.config().name == label) {
            return Some(tr::lookup_id(TrId::CommonCreateAccountsanitizedRepeatedLabel).to_string());
        }

        if label.trim().is_empty() {
            return Some(tr::lookup_id(TrId::CommonCreateAccountsanitizedEmptyLabel).to_string());
        }

        None
    }

    pub fn validate_index_string(&self, index: &str, network: NgNetwork) -> Option<String> {
        let index = match Self::parse_index_string(index) {
            Ok(Some(index)) => index,
            Ok(None) => return None,
            Err(message) => return Some(message),
        };

        self.validate_index(index, network)
    }

    pub fn parse_index_string(index: &str) -> Result<Option<u32>, String> {
        let index = index.trim();
        if index.is_empty() {
            return Ok(None);
        }

        match index.parse::<u64>() {
            Ok(index) if index <= Self::MAX_BIP32_INDEX as u64 => Ok(Some(index as u32)),
            _ => Err(tr::lookup_id(TrId::CommonCreateAccountsanitizedCappedIndex).to_string()),
        }
    }

    pub fn validate_index(&self, index: u32, network: NgNetwork) -> Option<String> {
        if index > Self::MAX_BIP32_INDEX {
            return Some(tr::lookup_id(TrId::CommonCreateAccountsanitizedCappedIndex).to_string());
        }
        self.active_accounts()
            .filter_map(|(_id, a)| {
                if a.multisig.is_none() && a.network == network && a.index == index {
                    if a.archived {
                        return Some(tr::lookup_id(TrId::CommonCreateAccountIndexArchived).to_string());
                    }
                    return Some(tr::lookup_id(TrId::CommonCreateAccountIndexUsed).to_string());
                }

                None
            })
            .next()
    }

    pub fn get_next_index(&self, network: NgNetwork) -> u32 {
        let mut taken_indices = self
            .active_accounts()
            .filter_map(|(_id, config)| {
                if config.multisig.is_none() && config.network == network {
                    return Some(config.index);
                }
                None
            })
            .collect::<Vec<u32>>();
        taken_indices.sort();

        // Find the first space in the sortted list of taken account indices
        // This should only happen if the user manually adds custom accounts that cause a gap in the range.
        // Otherwise, the next_index will be incremented up to the number of accounts.
        let mut next_index: u32 = 0;
        for i in taken_indices.iter() {
            if next_index != *i {
                break;
            } else {
                next_index += 1;
            }
        }

        next_index
    }

    pub fn active_accounts(&self) -> impl Iterator<Item = (&AccountId, ConfigBorrow<'_>)> {
        self.accounts
            .iter()
            .filter(|(id, a)| match id {
                AccountId::Single { fingerprint, .. } => fingerprint == &self.fingerprint,
                AccountId::Multi { .. } => match &a.config().multisig {
                    Some(m) => {
                        m.get_signers().iter().find(|s| s.get_fingerprint() == self.fingerprint).is_some()
                    }
                    None => false,
                },
            })
            .map(|(k, v)| (k, v.config()))
    }

    pub fn load_master_key(&self, network: NgNetwork) -> anyhow::Result<MasterKey> {
        load_master_key(&self.secp, &self.security, self.passphrase.as_str(), network)
    }

    /// Switches between Default and Passphrase views using cached fingerprints
    /// Unlike set_passphrase, this never reads from the SE
    pub fn switch_fingerprint(&mut self, passphrase: String) {
        if self.passphrase == passphrase {
            return;
        }

        self.unload_passphrase_accounts();

        self.passphrase = passphrase;
        self.fingerprint = if self.passphrase.is_empty() {
            self.default_fingerprint
        } else {
            self.passphrase_fingerprint.unwrap_or(self.default_fingerprint)
        };
    }

    fn unload_passphrase_accounts(&mut self) {
        let has_passphrase = self.has_passphrase();
        for (_id, acc) in self.accounts.iter_mut().filter(|(id, _a)| match id {
            AccountId::Single { fingerprint, .. } => has_passphrase && fingerprint == &self.fingerprint,
            // Always unload all multisigs, see SFT-6077
            AccountId::Multi { .. } => true,
        }) {
            acc.unload();
        }
    }

    /// Applies a pre-computed fingerprint after an async SE read. Handles account
    /// unloading and caching without touching the SE itself
    pub fn apply_fingerprint(&mut self, passphrase: String, fingerprint: Fingerprint) {
        self.unload_passphrase_accounts();
        self.passphrase = passphrase;
        self.fingerprint = fingerprint;
        if self.has_passphrase() {
            self.passphrase_fingerprint = Some(fingerprint);
        } else {
            self.passphrase_fingerprint = None;
        }
    }

    pub fn delete_account(&mut self, id: AccountId) {
        if let Some(_acct) = self.accounts.remove(&id) {
            delete_account_files(&id).unwrap_or_else(|e| {
                log::error!("{:?}", e);
            });
        }
    }
}

/// Computes the fingerprint for a passphrase by reading from the SE
/// Creates its own Security and Secp instances so it can run in a worker thread
pub fn fingerprint_for_passphrase(passphrase: &str) -> anyhow::Result<Fingerprint> {
    let secp = Secp256k1::new();
    let security = crate::Security::default();
    load_master_key(&secp, &security, passphrase, NgNetwork::Bitcoin).map(|m| m.fingerprint)
}

fn load_master_key(
    secp: &Secp256k1<All>,
    security: &crate::Security,
    passphrase: &str,
    network: NgNetwork,
) -> anyhow::Result<MasterKey> {
    let entropy = security
        .seed()
        .context("failed to retrieve seed")?
        .ok_or_else(|| anyhow::anyhow!("no seed available"))?;
    let master_key = MasterKey::from_entropy(secp, network, entropy.bytes(), passphrase, None)
        .context("Failed to calculate master key")?;
    Ok(master_key)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        ngwallet::config::{AddressType, MultiSigSigner},
    };

    fn signer(
        secp: &Secp256k1<All>,
        key: &MasterKey,
        fingerprint: Fingerprint,
        derivation: &str,
    ) -> MultiSigSigner {
        let derivation: DerivationPath = derivation.parse().unwrap();
        let root = SensitiveXpriv(Xpriv::new_master(NgNetwork::Bitcoin, &key.key.0).unwrap());
        let derived = SensitiveXpriv(root.0.derive_priv(secp, &derivation).unwrap());
        MultiSigSigner::new(&derivation, &fingerprint, &Xpub::from_priv(secp, &derived.0))
    }

    fn details(signers: Vec<MultiSigSigner>) -> MultiSigDetails {
        MultiSigDetails::new(2, 2, AddressType::P2ShWsh, Some(NetworkKind::Main), signers).unwrap()
    }

    #[test]
    fn casa_multisig_requires_the_local_xpub_for_its_fingerprint() {
        let secp = Secp256k1::new();
        let local = MasterKey::from_entropy(&secp, NgNetwork::Bitcoin, &[0; 16], "", None).unwrap();
        let attacker = MasterKey::from_entropy(&secp, NgNetwork::Bitcoin, &[1; 16], "", None).unwrap();
        let cosigner = MasterKey::from_entropy(&secp, NgNetwork::Bitcoin, &[2; 16], "", None).unwrap();

        let valid = details(vec![
            signer(&secp, &local, local.fingerprint, "m/45'/0/0"),
            signer(&secp, &cosigner, cosigner.fingerprint, "m/45'/0/0"),
        ]);
        validate_local_multisig_signer(&secp, &local, &valid).unwrap();

        let spoofed = details(vec![
            signer(&secp, &attacker, local.fingerprint, "m/45'/0/0"),
            signer(&secp, &cosigner, cosigner.fingerprint, "m/45'/0/0"),
        ]);
        assert!(validate_local_multisig_signer(&secp, &local, &spoofed).is_err());
    }

    #[test]
    fn casa_multisig_accepts_legacy_and_new_account_derivations() {
        let secp = Secp256k1::new();
        let local = MasterKey::from_entropy(&secp, NgNetwork::Bitcoin, &[0; 16], "", None).unwrap();
        let cosigner = MasterKey::from_entropy(&secp, NgNetwork::Bitcoin, &[1; 16], "", None).unwrap();

        for derivation in ["m/49/0/7", "m/45'/0/7"] {
            let multisig = details(vec![
                signer(&secp, &local, local.fingerprint, derivation),
                signer(&secp, &cosigner, cosigner.fingerprint, derivation),
            ]);
            validate_local_multisig_signer(&secp, &local, &multisig).unwrap();
        }

        let unrelated = details(vec![
            signer(&secp, &local, local.fingerprint, "m/84'/0'/0'"),
            signer(&secp, &cosigner, cosigner.fingerprint, "m/84'/0'/0'"),
        ]);
        assert!(validate_local_multisig_signer(&secp, &local, &unrelated).is_err());
    }

    #[test]
    fn casa_multisig_ignores_reconstructed_xpub_metadata() {
        let secp = Secp256k1::new();
        let local = MasterKey::from_entropy(&secp, NgNetwork::Bitcoin, &[0; 16], "", None).unwrap();
        let cosigner = MasterKey::from_entropy(&secp, NgNetwork::Bitcoin, &[1; 16], "", None).unwrap();
        let derivation: DerivationPath = "m/45'/0/0".parse().unwrap();
        let root = SensitiveXpriv(Xpriv::new_master(NgNetwork::Bitcoin, &local.key.0).unwrap());
        let derived = SensitiveXpriv(root.0.derive_priv(&secp, &derivation).unwrap());
        let mut reconstructed = Xpub::from_priv(&secp, &derived.0);
        reconstructed.depth = 0;
        reconstructed.parent_fingerprint = Fingerprint::default();
        reconstructed.child_number = ChildNumber::Normal { index: 0 };

        let multisig = details(vec![
            MultiSigSigner::new(&derivation, &local.fingerprint, &reconstructed),
            signer(&secp, &cosigner, cosigner.fingerprint, "m/45'/0/0"),
        ]);
        validate_local_multisig_signer(&secp, &local, &multisig).unwrap();
    }
}
