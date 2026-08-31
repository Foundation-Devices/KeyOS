// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, RwLock},
    time::Instant,
};

use anyhow::Context;
use ngwallet::{
    account::NgAccount,
    bdk_wallet::{
        bitcoin::secp256k1::{Secp256k1, Signing},
        ChangeSet, KeychainKind, WalletPersister,
    },
    bip39::MasterKey,
    config::{AddressType, NgAccountConfig, NgDescriptor},
    ngwallet::NgWallet,
    store::MetaStorage,
};
use slint_keyos_platform::file_backed::JsonBacked;

use crate::{
    account_id::AccountId,
    fs_permissions::FileSystemPermissions,
    get_timestamp_in_milliseconds, log_ms,
    state::AccountColor,
    store::{
        AccountSource, CreateMultiSigAccount, CreateSingleSigAccount, ACCOUNT_SOURCE_TAG,
        CASA_ACCOUNT_SOURCE, UNCHAINED_ACCOUNT_SOURCE,
    },
    FileSystem,
};

/// # File Structure
/// The accounts are stored in the following structure:
/// ```ignore
/// <account_id>/
/// ├── database.json
/// ├── wallet_<...>.json
/// └── ... (additional wallet files)
/// ... (additional account directories)
/// ```
///
/// Where:
/// * `<account_id>` is one of:
///    - "Wallet_<Fingerprint>_<Network>_SingleSig_<index>"
///    - "Wallet_<Network>_MultiSig_<title>"
/// * `database` is the account's storage backend
/// * Each `wallet_<...>.json` file contains a wallet's changeset data, with multiple wallets possible per
///   account
/// * Multisig details are displayed when the current fingerprint matches a multisig member.
/// * Multisig wallets are used for balance persistence and address verification/exploration, signing is done
///   with a singlesig member of a multisig.

const ACCOUNTS_DIR: &str = "accounts";
const ACCOUNT_DB_FILE: &str = "database.json";

pub async fn load_account_configs(
    tx: async_channel::Sender<(AccountId, NgAccountConfig, Arc<dyn MetaStorage + Send>)>,
) -> anyhow::Result<()> {
    let start = Instant::now();

    let fs = FileSystem::default();
    let accounts_dir =
        fs.create_dir(ACCOUNTS_DIR, fs::Location::AppData).context("failed to create accounts dir")?;
    while let Some(entry) = accounts_dir
        .next_entry()
        .inspect_err(|e| log::error!("failed to get next account dir entry {e:?}"))
        .ok()
        .flatten()
    {
        let name = entry.name.as_str();
        if entry.is_dir && !name.is_empty() && !name.starts_with(".") {
            let account_id = match name.parse::<AccountId>() {
                Ok(account_id) => account_id,
                Err(e) => {
                    log::warn!("failed to parse account id {name} {e:?}");
                    continue;
                }
            };
            match load_account_config(&account_id) {
                Ok((storage, account)) => {
                    let _ = tx.send((account_id, account, storage)).await;
                }
                Err(e) => log::error!("Failed to load account {name}: {e:?}"),
            }
        }
    }

    log::info!("loaded all account configs in {}ms", start.elapsed().as_millis());
    Ok(())
}

pub fn delete_account_files(account_id: &AccountId) -> anyhow::Result<()> {
    let start = Instant::now();

    let fs = FileSystem::default();
    match fs.remove(format!("{ACCOUNTS_DIR}/{account_id}/"), fs::Location::AppData) {
        Ok(_) => {
            log::info!("deleted account {} in {}ms", account_id, start.elapsed().as_millis());
            Ok(())
        }
        Err(e) => Err(e).context(format!("failed to delete account {}", account_id)),
    }
}

pub fn build_singlesig_account(
    fs: &FileSystem,
    master_key: MasterKey,
    device_serial: String,
    passphrase: String,
    create: CreateSingleSigAccount,
) -> anyhow::Result<(AccountId, NgAccount<KeyOsWalletPersister>)> {
    let account_id = AccountId::new_single(master_key.fingerprint, create.network, create.index);
    let descriptors = log_ms("getting descriptors", || {
        ngwallet::bip39::get_descriptors(&master_key.key.0, create.network, create.index)
    })
    .context("failed to load descriptors")?;

    let meta_storage = create_meta_storage(&account_id);
    let descriptors = log_ms("open_descriptors", || {
        open_descriptors(true, &fs, &account_id, descriptors, create.network, meta_storage.clone())
    })?;

    let (wallets, descriptors) = descriptors.into_iter().unzip();

    let config = NgAccountConfig {
        id: account_id.to_string(),
        name: create.label,
        index: create.index,
        network: create.network,
        color: AccountColor::LightCopper.to_hex().to_string(),
        date_added: Some(get_timestamp_in_milliseconds()),
        seed_has_passphrase: !passphrase.is_empty(),
        device_serial: Some(device_serial),

        descriptors,

        preferred_address_type: ngwallet::config::AddressType::P2wpkh,
        date_synced: None,
        multisig: None,
        archived: false,
        last_remote_sequence: 0,
    };

    meta_storage.set_config(config.serialize().as_str()).context("set account config")?;
    meta_storage.persist().context("persist account config")?;
    let account = account_from_parts(config, wallets, meta_storage);

    Ok((account_id, account))
}

fn account_from_parts(
    config: NgAccountConfig,
    wallets: Vec<NgWallet<KeyOsWalletPersister>>,
    meta_storage: Arc<dyn MetaStorage>,
) -> NgAccount<KeyOsWalletPersister> {
    NgAccount { config: Arc::new(RwLock::new(config)), wallets: Arc::new(RwLock::new(wallets)), meta_storage }
}

pub fn build_multisig_account(
    fs: &FileSystem,
    secp: &Secp256k1<impl Signing>,
    master_key: MasterKey,
    device_serial: String,
    create: CreateMultiSigAccount,
) -> anyhow::Result<(AccountId, NgAccount<KeyOsWalletPersister>)> {
    let account_id = AccountId::new_multi(&create.multisig, create.network);
    let descriptors =
        create.multisig.get_descriptors(secp, Some(&master_key)).context("get multisig descriptors")?;
    let meta_storage = create_meta_storage(&account_id);
    let descriptors =
        open_descriptors(true, fs, &account_id, descriptors, create.network, meta_storage.clone())
            .context("opening multisig descriptors")?;
    let (wallets, descriptors) = descriptors.into_iter().unzip();

    let config = NgAccountConfig {
        id: account_id.to_string(),
        name: create.label,
        network: create.network,
        color: create.color.to_hex().to_string(),
        date_added: Some(get_timestamp_in_milliseconds()),
        multisig: Some(create.multisig),
        device_serial: Some(device_serial),

        descriptors,

        preferred_address_type: ngwallet::config::AddressType::P2wpkh,
        index: 0,
        seed_has_passphrase: false,
        date_synced: None,
        archived: false,
        last_remote_sequence: 0,
    };

    if let Some(source) = match create.source {
        AccountSource::Generic => None,
        AccountSource::Casa => Some(CASA_ACCOUNT_SOURCE),
        AccountSource::Unchained => Some(UNCHAINED_ACCOUNT_SOURCE),
    } {
        meta_storage.set_tag(ACCOUNT_SOURCE_TAG, source).context("set account source")?;
    }
    meta_storage.set_config(config.serialize().as_str()).context("set account config")?;
    meta_storage.persist().context("persist account config")?;
    let account = account_from_parts(config, wallets, meta_storage);

    Ok((account_id, account))
}

pub fn load_account_config(
    account_id: &AccountId,
) -> anyhow::Result<(Arc<dyn MetaStorage>, NgAccountConfig)> {
    let path = meta_storage_path(account_id);
    let storage = Arc::new(KeyOsMetaStorage::load(path)?);
    let config =
        storage.get_config()?.ok_or_else(|| anyhow::anyhow!("Could not read account config from storage"))?;
    Ok((storage, config))
}

pub fn load_account(
    fs: &FileSystem,
    account_id: &AccountId,
    config: NgAccountConfig,
    meta_storage: Arc<dyn MetaStorage>,
    descriptors: Vec<ngwallet::bip39::Descriptors>,
) -> anyhow::Result<NgAccount<KeyOsWalletPersister>> {
    let wallets = open_descriptors(false, fs, account_id, descriptors, config.network, meta_storage.clone())?
        .into_iter()
        .map(|(wallet, _)| wallet)
        .collect();
    let account = account_from_parts(config, wallets, meta_storage);
    Ok(account)
}

pub fn create_meta_storage(account_id: &AccountId) -> Arc<dyn MetaStorage> {
    let path = meta_storage_path(account_id);
    Arc::new(KeyOsMetaStorage::new(path))
}

fn meta_storage_path(account_id: &AccountId) -> String {
    format!("{ACCOUNTS_DIR}/{account_id}/{ACCOUNT_DB_FILE}")
}

pub fn open_descriptors(
    create_new: bool,
    fs: &FileSystem,
    account_id: &AccountId,
    descriptors: Vec<ngwallet::bip39::Descriptors>,
    network: ngwallet::bdk_wallet::bitcoin::Network,
    meta_storage: Arc<dyn MetaStorage>,
) -> anyhow::Result<Vec<(NgWallet<KeyOsWalletPersister>, NgDescriptor)>> {
    let _ = fs
        .create_dir(format!("{ACCOUNTS_DIR}/{account_id}"), fs::Location::AppData)
        .context("failed to create account dir")?;

    let mut wallets = vec![];

    for d in descriptors {
        let bip = d.bip();
        let name = format!("{ACCOUNTS_DIR}/{account_id}/wallet_bip_{bip}.json");
        let persister = KeyOsWalletPersister::new(name)?;

        let wallet = {
            let internal = d.change_descriptor.clone();
            let external = d.descriptor.clone();
            let persister = Arc::new(Mutex::new(persister));

            if create_new {
                NgWallet::new_from_descriptor(
                    internal,
                    Some(external),
                    network,
                    meta_storage.clone(),
                    persister,
                )
                .context("new wallet")?
            } else {
                NgWallet::load(internal, Some(external), meta_storage.clone(), persister)
                    .context("load wallet")?
            }
        };
        let change_xpub = d.change_descriptor_xpub();

        let config_descriptor = NgDescriptor {
            address_type: ngwallet::utils::get_address_type(&change_xpub),
            internal: change_xpub,
            external: Some(d.descriptor_xpub()),
            export_addr_hint: Some(d.export_addr_hint),
        };

        wallets.push((wallet, config_descriptor))
    }

    Ok(wallets)
}

#[derive(Debug)]
pub struct KeyOsMetaStorage {
    data: Mutex<JsonBacked<AccountMetaStorage, FileSystemPermissions>>,
}

#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct AccountMetaStorage {
    config: Option<String>,
    notes: BTreeMap<String, String>,
    tags: BTreeMap<String, String>,
    tag_list: BTreeMap<String, String>,
    do_not_spend: BTreeMap<String, bool>,
    last_verified_address: BTreeMap<AddressKey, u32>,
    fees: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct AddressKey(AddressType, KeychainKind);

impl serde::Serialize for AddressKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let address_type = match self.0 {
            AddressType::P2pkh => 0,
            AddressType::P2sh => 1,
            AddressType::P2wpkh => 2,
            AddressType::P2wsh => 3,
            AddressType::P2tr => 4,
            AddressType::P2ShWpkh => 5,
            AddressType::P2ShWsh => 6,
            _ => 0,
        };
        let keychain = match self.1 {
            KeychainKind::External => 0,
            KeychainKind::Internal => 1,
        };
        serializer.serialize_str(&format!("{address_type}:{keychain}"))
    }
}

impl<'de> serde::Deserialize<'de> for AddressKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let (address_type_str, keychain_str) =
            s.split_once(':').ok_or_else(|| serde::de::Error::custom("invalid AddressKey format"))?;

        let address_type =
            address_type_str.parse::<u8>().map_err(|_| serde::de::Error::custom("invalid address type"))?;
        let keychain =
            keychain_str.parse::<u8>().map_err(|_| serde::de::Error::custom("invalid keychain"))?;

        let address_type = match address_type {
            0 => AddressType::P2pkh,
            1 => AddressType::P2sh,
            2 => AddressType::P2wpkh,
            3 => AddressType::P2wsh,
            4 => AddressType::P2tr,
            5 => AddressType::P2ShWpkh,
            6 => AddressType::P2ShWsh,
            _ => return Err(serde::de::Error::custom("invalid address type")),
        };

        let keychain = match keychain {
            0 => KeychainKind::External,
            1 => KeychainKind::Internal,
            _ => return Err(serde::de::Error::custom("invalid keychain kind")),
        };

        Ok(AddressKey(address_type, keychain))
    }
}

impl KeyOsMetaStorage {
    pub fn new(path: String) -> Self {
        let data = JsonBacked::new(path, fs::Location::AppData).0;
        Self { data: Mutex::new(data) }
    }

    pub fn load(path: String) -> anyhow::Result<Self> {
        let storage = JsonBacked::load(&path, fs::Location::AppData)
            .with_context(|| anyhow::anyhow!("missing metastorage file {path}"))?;
        Ok(Self { data: Mutex::new(storage) })
    }
}

impl MetaStorage for KeyOsMetaStorage {
    fn set_fee(&self, txid: &str, fee: u64) -> anyhow::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.guard().fees.insert(txid.to_string(), fee);
        Ok(())
    }

    fn get_fee(&self, txid: &str) -> anyhow::Result<Option<u64>> {
        let data = self.data.lock().unwrap();
        Ok(data.fees.get(txid).copied())
    }

    fn set_note(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.guard().notes.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn get_note(&self, key: &str) -> anyhow::Result<Option<String>> {
        let data = self.data.lock().unwrap();
        Ok(data.notes.get(key).cloned())
    }

    fn list_tags(&self) -> anyhow::Result<Vec<String>> {
        let data = self.data.lock().unwrap();
        Ok(data.tag_list.values().cloned().collect())
    }

    fn add_tag(&self, tag: &str) -> anyhow::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.guard().tag_list.insert(tag.to_lowercase(), tag.to_string());
        Ok(())
    }

    fn remove_tag(&self, tag: &str) -> anyhow::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.guard().tag_list.remove(&tag.to_lowercase());
        Ok(())
    }

    fn set_tag(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.guard().tags.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn get_tag(&self, key: &str) -> anyhow::Result<Option<String>> {
        let data = self.data.lock().unwrap();
        Ok(data.tags.get(key).cloned())
    }

    fn set_do_not_spend(&self, key: &str, value: bool) -> anyhow::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.guard().do_not_spend.insert(key.to_string(), value);
        Ok(())
    }

    fn get_do_not_spend(&self, key: &str) -> anyhow::Result<bool> {
        let data = self.data.lock().unwrap();
        Ok(data.do_not_spend.get(key).copied().unwrap_or(false))
    }

    fn set_config(&self, deserialized_config: &str) -> anyhow::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.guard().config = Some(deserialized_config.to_string());
        Ok(())
    }

    fn get_config(&self) -> anyhow::Result<Option<NgAccountConfig>> {
        let data = self.data.lock().unwrap();
        if let Some(config_str) = &data.config {
            let config: NgAccountConfig = serde_json::from_str(config_str)?;
            Ok(Some(config))
        } else {
            Ok(None)
        }
    }

    fn set_last_verified_address(
        &self,
        address_type: AddressType,
        keychain: KeychainKind,
        index: u32,
    ) -> anyhow::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.guard().last_verified_address.insert(AddressKey(address_type, keychain), index);
        Ok(())
    }

    fn get_last_verified_address(
        &self,
        address_type: AddressType,
        keychain: KeychainKind,
    ) -> anyhow::Result<u32> {
        let data = self.data.lock().unwrap();
        Ok(data.last_verified_address.get(&AddressKey(address_type, keychain)).copied().unwrap_or(0))
    }

    fn persist(&self) -> anyhow::Result<bool> {
        // Nothing to do: every mutation went out to disk when its guard dropped.
        Ok(true)
    }
}

#[derive(Debug)]
pub struct KeyOsWalletPersister {
    file: JsonBacked<ChangeSet, FileSystemPermissions>,
}

impl KeyOsWalletPersister {
    pub fn new(name: String) -> anyhow::Result<Self> {
        let (file, _restored) = JsonBacked::new(name, fs::Location::AppData);
        Ok(Self { file })
    }
}

impl WalletPersister for KeyOsWalletPersister {
    type Error = anyhow::Error;

    fn initialize(persister: &mut Self) -> Result<ChangeSet, Self::Error> {
        log_ms("initialize wallet", || Ok(persister.file.0.clone()))
    }

    fn persist(persister: &mut Self, changeset: &ChangeSet) -> Result<(), Self::Error> {
        log_ms("persist wallet", || {
            persister.file.guard().0 = changeset.clone();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_account_meta_storage_encode_decode_roundtrip() {
        let mut storage = AccountMetaStorage::default();
        storage.config = Some("test config".to_string());
        storage.notes.insert("tx1".to_string(), "note1".to_string());
        storage.tags.insert("tx2".to_string(), "tag1".to_string());
        storage.tag_list.insert("tag1".to_string(), "Tag One".to_string());
        storage.do_not_spend.insert("utxo1".to_string(), true);
        storage.last_verified_address.insert(AddressKey(AddressType::P2wpkh, KeychainKind::Internal), 42);
        storage.last_verified_address.insert(AddressKey(AddressType::P2sh, KeychainKind::External), 42);
        storage.fees.insert("tx3".to_string(), 1000);

        let encoded = serde_json::to_vec(&storage).expect("encoding failed");
        let decoded: AccountMetaStorage = serde_json::from_slice(&encoded).expect("decoding failed");

        assert_eq!(storage, decoded);
    }
}
