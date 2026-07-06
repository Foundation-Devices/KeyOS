// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        account_id::AccountId, state::AppState, ExportAccount, ExportAccountState, ExportCapabilities,
        ExportFormats,
    },
    foundation_urtypes::value::Value as UrValue,
    ngwallet::{
        bdk_wallet::bitcoin::{base58, Network as NgNetwork},
        config::{AddressType as NgAddressType, NgAccountConfig},
        utils::extract_xpub_from_descriptor,
    },
    security::OsVersionInfo,
    serde::{Deserialize, Serialize},
    slint_keyos_platform::{
        slint::{ComponentHandle, ModelRc, SharedString, VecModel},
        spawn_local, StoredValue,
    },
    std::{collections::BTreeMap, fmt::Debug, io::Write, rc::Rc},
};

// mod bitcoin_core;
mod bitcoin_keeper;
mod bitcoin_safe;
mod blue_wallet;
mod btcpay;
mod bull;
// mod casa;
mod coconut_wallet;
mod coinbits;
mod electrum;
// mod envoy;
mod fully_noded;
mod nunchuk;
mod sparrow;
mod specter;
mod theya;
mod zeus;

// This is done for macro purposes
use {
    // bitcoin_core::CONNECTOR as BitcoinCore,
    bitcoin_keeper::CONNECTOR as BitcoinKeeper,
    bitcoin_safe::CONNECTOR as BitcoinSafe,
    blue_wallet::CONNECTOR as BlueWallet,
    btcpay::CONNECTOR as BtcPay,
    bull::CONNECTOR as Bull,
    // casa::CONNECTOR as Casa,
    coconut_wallet::CONNECTOR as CoconutWallet,
    coinbits::CONNECTOR as Coinbits,
    electrum::CONNECTOR as Electrum,
    // envoy::CONNECTOR as Envoy,
    fully_noded::CONNECTOR as FullyNoded,
    nunchuk::CONNECTOR as Nunchuk,
    sparrow::CONNECTOR as Sparrow,
    specter::CONNECTOR as Specter,
    theya::CONNECTOR as Theya,
    zeus::CONNECTOR as Zeus,
};

const MULTISIGS_DIR: &str = "multisig_configs/";
const WALLETS_DIR: &str = "wallet_configs/";

pub fn init(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let global = ui.global::<ExportAccount>();

    global.on_all_connectors(|| {
        ModelRc::new(VecModel::from(
            all_connector_names().into_iter().map(SharedString::from).collect::<Vec<_>>(),
        ))
    });

    global.on_connector_display_name(|connector_id| {
        let connector = match get_connector(&connector_id) {
            Ok(c) => c,
            Err(e) => {
                log::error!("unable to get connector: {e}");
                return SharedString::new();
            }
        };

        connector.display_name()
    });

    global.on_connector_capabilities(|connector_id| {
        let connector = match get_connector(&connector_id) {
            Ok(c) => c,
            Err(e) => {
                log::error!("unable to get connector: {e}");
                return Default::default();
            }
        };

        connector.capabilities()
    });

    global.on_connector_formats(|connector_id| {
        let connector = match get_connector(&connector_id) {
            Ok(c) => c,
            Err(e) => {
                log::error!("unable to get connector: {e}");
                return Default::default();
            }
        };

        connector.formats()
    });

    // Export callbacks using string-based interface
    global.on_export_account_qr({
        move |id, connector_id, as_multi, density| {
            let account_id = match id.parse::<AccountId>() {
                Ok(acct) => acct,
                Err(e) => {
                    log::error!("failed to parse account id {id} {e:?}");
                    return Default::default();
                }
            };

            let connector = match get_connector(&connector_id) {
                Ok(c) => c,
                Err(e) => {
                    log::error!("unable to get connector: {e}");
                    return Default::default();
                }
            };

            let capabilities = connector.capabilities();
            if !capabilities.single && !as_multi {
                log::error!("single requested but not supported for {}", connector_id);
                return Default::default();
            }

            if !capabilities.join_multisig && as_multi {
                log::error!("join multisig requested but not supported for {}", connector_id);
                return Default::default();
            }

            let app_state = state.borrow();

            let ng_account_config = match app_state.store.get_account_config(&account_id) {
                Some(account_config) => account_config,
                None => {
                    log::error!("Failed to get account {} for multisig config export", account_id);
                    return Default::default();
                }
            };

            let content = match connector.connect(&app_state, &account_id, &*ng_account_config, as_multi) {
                Ok(c) => c,
                Err(e) => {
                    log::error!(
                        "Could not get {} export for {}: {:?}",
                        if as_multi { "multi" } else { "single" },
                        connector_id,
                        e
                    );
                    return Default::default();
                }
            };

            // 0 density indicates a single QR code
            match density {
                0 => ModelRc::from(Rc::new(VecModel::from(vec![SharedString::from(content)]))),
                _ => {
                    let cbor = match minicbor::to_vec(UrValue::Bytes(&content.into_bytes())) {
                        Ok(b) => b,
                        Err(e) => {
                            log::error!("Could not serialize multisig config: {:?}", e);
                            return Default::default();
                        }
                    };

                    slint_keyos_platform::qrcode::encode_qr_parts("bytes", cbor, density)
                }
            }
        }
    });

    // File export callbacks
    global.on_export_account_file(move |id, connector_id, as_multi| {
        spawn_local(async move {
            export_account_file(state, id, connector_id, as_multi).await;
        })
        .detach();
    });

    global.on_export_multisig_config_qr({
        move |id, density| {
            let account_id = match id.parse::<AccountId>() {
                Ok(acct) => acct,
                Err(e) => {
                    log::error!("failed to parse account id {id} {e:?}");
                    return Default::default();
                }
            };

            let app_state = state.borrow();

            let ng_account_config = match app_state.store.get_account_config(&account_id) {
                Some(account_config) => account_config,
                None => {
                    log::error!("Failed to get account {} for multisig config export", account_id);
                    return Default::default();
                }
            };

            let content = match &ng_account_config.multisig {
                // TODO: potentially map to different config types like
                // BSMS, JSON, and Descriptor by wallet in the future
                Some(m) => m.to_config(ng_account_config.name.clone()),
                None => {
                    log::error!("Account {} is not multisig", account_id);
                    return Default::default();
                }
            };

            let cbor = match minicbor::to_vec(UrValue::Bytes(&content.into_bytes())) {
                Ok(b) => b,
                Err(e) => {
                    log::error!("Could not serialize multisig config: {:?}", e);
                    return Default::default();
                }
            };

            slint_keyos_platform::qrcode::encode_qr_parts("bytes", cbor, density)
        }
    });

    global.on_export_multisig_config_file(move |id| {
        spawn_local(async move {
            export_multisig_file(state, id).await;
        })
        .detach();
    });
}

fn set_error(
    global: ExportAccount<'_>,
    error_title: impl Into<String>,
    error_text: impl Into<String>,
    error: Option<impl Debug>,
) {
    let error_title: String = error_title.into();
    let error_text: String = error_text.into();
    log::error!(
        "{}: {}{}",
        error_title,
        error_text,
        error.map(|e| format!(", {:?}", e)).unwrap_or(String::new())
    );
    global.set_state(ExportAccountState::Error);
}

async fn export_multisig_file(state: StoredValue<AppState>, id: SharedString) {
    let app_state = state.borrow();
    let ui = app_state.ui();
    let global = ui.global::<ExportAccount>();

    global.set_state(ExportAccountState::Saving);

    let account_id = match id.parse::<AccountId>() {
        Ok(acct) => acct,
        Err(e) => {
            set_error(global, "Could not save file", format!("Failed to parse account id: {}", id), Some(e));
            return;
        }
    };

    let ng_account_config = match app_state.store.get_account_config(&account_id) {
        Some(account_config) => account_config,
        None => {
            set_error(
                global,
                "Could not save file",
                format!("Failed to get account {} for multisig config export", id),
                None::<()>,
            );
            return;
        }
    };

    let content = match &ng_account_config.multisig {
        // TODO: potentially map to different config types like
        // BSMS, JSON, and Descriptor by wallet in the future
        Some(m) => m.to_config(ng_account_config.name.clone()),
        None => {
            set_error(
                global,
                "Could not save file",
                format!("Account {} is not multisig", account_id),
                None::<()>,
            );
            return;
        }
    };

    let multisigs_dir = match app_state.store.fs.create_dir(MULTISIGS_DIR, fs::Location::Airlock) {
        Ok(d) => d,
        Err(e) => {
            set_error(global, "Could not save file", "Could not open or create multisigs directory", Some(e));
            return;
        }
    };

    // TODO: this and below could be a common flow or a filesystem function
    let filename = format!("{}.txt", ng_account_config.name.clone());
    let filename = match multisigs_dir.pick_next_filename(&filename, None) {
        Ok(f) => f,
        Err(e) => {
            set_error(
                global,
                "Could not save file",
                format!("Could not get a filename for {}", filename),
                Some(e),
            );
            return;
        }
    };

    let path = format!("{}{}", MULTISIGS_DIR, filename);
    let mut file = match app_state.store.fs.open_file(
        &path,
        fs::Location::Airlock,
        fs::OpenFlags { read: false, write: true, create: true },
    ) {
        Ok(f) => f,
        Err(e) => {
            set_error(
                global,
                "Could not save file",
                format!("Failed to create file '{}'", filename),
                Some(e),
            );
            return;
        }
    };

    match file.overwrite(content.as_bytes()) {
        Ok(_) => log::info!("Successfully exported account {} to file '{}'", account_id, filename),
        Err(e) => {
            set_error(
                global,
                "Could not save file",
                format!("Failed to write content to file '{}'", filename),
                Some(e),
            );
            return;
        }
    }

    global.set_saved_file_path(path.into());
    global.set_state(ExportAccountState::Saved);
}

async fn export_account_file(
    state: StoredValue<AppState>,
    id: SharedString,
    connector_id: SharedString,
    as_multi: bool,
) {
    let app_state = state.borrow();
    let ui = app_state.ui();
    let global = ui.global::<ExportAccount>();

    global.set_state(ExportAccountState::Saving);

    let account_id = match id.parse::<AccountId>() {
        Ok(acct) => acct,
        Err(e) => {
            set_error(global, "Could not save file", format!("Failed to parse account id: {}", id), Some(e));
            return;
        }
    };

    let connector = match get_connector(&connector_id) {
        Ok(c) => c,
        Err(e) => {
            set_error(
                global,
                "Could not save file",
                format!("Unable to get connector for {}", connector_id),
                Some(e),
            );
            return;
        }
    };

    if !connector.formats().file {
        set_error(
            global,
            "Could not save file",
            format!("{} does not support file exports", connector.display_name()),
            None::<()>,
        );
        return;
    }

    let capabilities = connector.capabilities();
    if !capabilities.single && !as_multi {
        set_error(
            global,
            "Could not save file",
            format!("{} does not support single exports", connector.display_name()),
            None::<()>,
        );
        return;
    }

    if !capabilities.join_multisig && as_multi {
        set_error(
            global,
            "Could not save file",
            format!("{} does not support multi exports", connector.display_name()),
            None::<()>,
        );
        return;
    }

    let app_state = state.borrow();

    let ng_account_config = match app_state.store.get_account_config(&account_id) {
        Some(account_config) => account_config,
        None => {
            set_error(
                global,
                "Could not save file",
                format!("Failed to get account {} for export", id),
                None::<()>,
            );
            return;
        }
    };

    let content = match connector.connect(&app_state, &account_id, &*ng_account_config, as_multi) {
        Ok(c) => c,
        Err(e) => {
            set_error(
                global,
                "Could not save file",
                format!(
                    "Could not get {} export for {}",
                    if as_multi { "multi" } else { "single" },
                    connector_id
                ),
                Some(e),
            );
            return;
        }
    };

    let wallets_dir = match app_state.store.fs.create_dir(WALLETS_DIR, fs::Location::Airlock) {
        Ok(d) => d,
        Err(e) => {
            set_error(
                global,
                "Could not save file",
                "Could not open or create wallet exports directory",
                Some(e),
            );
            return;
        }
    };

    let filename = connector.export_filename(&account_id, as_multi);
    let filename = match wallets_dir.pick_next_filename(&filename, None) {
        Ok(f) => f,
        Err(e) => {
            set_error(
                global,
                "Could not save file",
                format!("Could not get a filename for {}", filename),
                Some(e),
            );
            return;
        }
    };

    let path = format!("{}{}", WALLETS_DIR, filename);
    let mut file = match app_state.store.fs.open_file(
        &path,
        fs::Location::Airlock,
        fs::OpenFlags { read: false, write: true, create: true },
    ) {
        Ok(f) => f,
        Err(e) => {
            set_error(
                global,
                "Could not save file",
                format!("Failed to create file '{}'", filename),
                Some(e),
            );
            return;
        }
    };

    match file.write_all(content.as_bytes()) {
        Ok(_) => log::info!("Successfully exported account {} to file '{}'", account_id, filename),
        Err(e) => {
            set_error(
                global,
                "Could not save file",
                format!("Failed to write content to file '{}'", filename),
                Some(e),
            );
            return;
        }
    }

    global.set_saved_file_path(path.into());
    global.set_state(ExportAccountState::Saved);
}

macro_rules! register_wallets {
    ( $( $Variant:ident ),+ $(,)? ) => {
        /// Get connector by string ID
        pub fn get_connector(connector_id: &str) -> Result<&'static dyn WalletConnector, anyhow::Error> {
            match connector_id {
                $( stringify!($Variant) => Ok(& $Variant as &'static dyn WalletConnector), )+
                _ => anyhow::bail!("Wallet is not supported: {:?}", connector_id),
            }
        }

        /// Get all connector names (internal names used as string IDs)
        pub fn all_connector_names() -> Vec<&'static str> {
            vec![
                $( stringify!($Variant), )+
            ]
        }
    };
}

register_wallets! {
    // Envoy,
    // BitcoinCore,
    BitcoinKeeper,
    BitcoinSafe,
    BlueWallet,
    BtcPay,
    Bull,
    // Casa,
    CoconutWallet,
    Coinbits,
    Electrum,
    FullyNoded,
    Nunchuk,
    Sparrow,
    Specter,
    Theya,
    Zeus,
}

pub trait WalletConnector {
    fn capabilities(&self) -> ExportCapabilities;
    fn formats(&self) -> ExportFormats;
    fn display_name(&self) -> SharedString;
    fn file_extension(&self, as_multi: bool) -> String;
    fn connect(
        &self,
        state: &AppState,
        id: &AccountId,
        cfg: &NgAccountConfig,
        as_multi: bool,
    ) -> Result<String, anyhow::Error>;

    fn export_filename(&self, id: &AccountId, as_multi: bool) -> String {
        let fingerprint = id.fingerprint().map(|f| format!("{}-", f)).unwrap_or(String::new());
        let capability = match as_multi {
            true => String::from("-multisig"),
            false => String::new(),
        };

        format!("{}{}{}.{}", fingerprint, self.display_name(), capability, self.file_extension(as_multi))
    }
}

// TODO: this should be a convenience function in security
fn get_version_info(state: &AppState) -> String {
    let Ok(version_info) = state.store.security.os_version_info() else {
        return String::new();
    };

    match version_info {
        None => String::new(),
        Some(OsVersionInfo { bootloader_version: _, keyos_version }) => {
            String::from_utf8_lossy(&keyos_version).to_string()
        }
    }
}

fn network_to_u32(network: NgNetwork) -> u32 {
    match network {
        NgNetwork::Bitcoin => 0,
        _ => 1,
    }
}

pub fn bip_from_addr_type(addr: &NgAddressType) -> (u32, Option<u32>) {
    match addr {
        NgAddressType::P2pkh => (44, None),
        NgAddressType::P2ShWpkh => (49, None),
        NgAddressType::P2wpkh => (84, None),
        NgAddressType::P2tr => (86, None),
        NgAddressType::P2ShWsh => (48, Some(1)),
        NgAddressType::P2wsh => (48, Some(2)),
        NgAddressType::P2sh => (48, Some(3)),
        _ => (84, None),
    }
}

pub fn name_from_addr_type(addr: &NgAddressType) -> &'static str {
    match addr {
        NgAddressType::P2pkh => "p2pkh",
        NgAddressType::P2ShWpkh => "p2sh-p2wpkh",
        NgAddressType::P2wpkh => "p2wpkh",
        NgAddressType::P2tr => "p2tr",
        NgAddressType::P2ShWsh => "p2sh-p2wsh",
        NgAddressType::P2wsh => "p2wsh",
        NgAddressType::P2sh => "p2sh",
        _ => "p2wpkh",
    }
}

pub fn name_from_addr_type_swapped(addr: &NgAddressType) -> &'static str {
    match addr {
        NgAddressType::P2pkh => "p2pkh",
        NgAddressType::P2ShWpkh => "p2wpkh-p2sh",
        NgAddressType::P2wpkh => "p2wpkh",
        NgAddressType::P2tr => "p2tr",
        NgAddressType::P2ShWsh => "p2wsh-p2sh",
        NgAddressType::P2wsh => "p2wsh",
        NgAddressType::P2sh => "p2sh",
        _ => "p2wpkh",
    }
}

pub fn convert_to_slip132_xpub(
    xpub_like: &str,
    network: NgNetwork,
    addr_type: &NgAddressType,
) -> Result<String, anyhow::Error> {
    let mut data = base58::decode_check(xpub_like).map_err(|_| anyhow::anyhow!("Invalid base58 in xpub"))?;

    if data.len() < 4 {
        return Err(anyhow::anyhow!("xpub too short"));
    }

    let slip132: [u8; 4] = match (network, addr_type) {
        (NgNetwork::Bitcoin, NgAddressType::P2wpkh) => [0x04, 0xB2, 0x47, 0x46], // zpub
        (NgNetwork::Bitcoin, NgAddressType::P2ShWpkh) => [0x04, 0x9D, 0x7C, 0xB2], // ypub
        (_, NgAddressType::P2wpkh) => [0x04, 0x5F, 0x1C, 0xF6],                  // vpub (testnet)
        (_, NgAddressType::P2ShWpkh) => [0x04, 0x4A, 0x52, 0x62],                // upub (testnet)
        _ => {
            log::warn!(
                "Unsupported address type {:?} for SLIP132 conversion, returning original xpub",
                addr_type
            );
            return Ok(xpub_like.to_string());
        }
    };
    data[0..4].copy_from_slice(&slip132);
    Ok(base58::encode_check(data.as_slice()))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct EnvoyPathFormat {
    derivation: String,
    // TODO: could this be a string?
    xfp: u32,
    xpub: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct EnvoyFormat {
    acct_name: String,
    acct_num: u32,
    // TODO: this is a float in Core, make sure this String works
    hw_version: String,
    fw_version: String,
    serial: String,
    device_name: String,
    color: String,
    #[serde(flatten)]
    paths: BTreeMap<String, EnvoyPathFormat>,
}

pub fn envoy_format(
    state: &AppState,
    id: &AccountId,
    cfg: &NgAccountConfig,
) -> Result<String, anyhow::Error> {
    let network_int = network_to_u32(cfg.network);

    let fpr = match id.fingerprint() {
        Some(f) => f.to_string(),
        None => anyhow::bail!("Could not get fingerprint for account id: {}", id),
    };

    let xfp = u32::from_str_radix(fpr.as_str(), 16).unwrap_or(0).swap_bytes();

    let paths = cfg
        .descriptors
        .iter()
        .filter_map(|d| {
            let addr_type = d.export_addr_hint.unwrap_or(d.address_type);
            let (bip_num, _) = bip_from_addr_type(&addr_type);

            if !vec![84u32, 86u32].contains(&bip_num) {
                return None;
            }

            let path = EnvoyPathFormat {
                derivation: format!("m/{}'/{}'/{}'", bip_num, network_int, cfg.index),
                xfp,
                xpub: extract_xpub_from_descriptor(&d.external.clone().unwrap_or_default()),
            };

            let name = format!("bip{}", bip_num);
            Some((name, path))
        })
        .collect();

    let envoy_data = EnvoyFormat {
        acct_name: cfg.name.clone(),
        acct_num: cfg.index,
        // TODO: update this to get prime's version
        hw_version: String::from("2"),
        fw_version: get_version_info(state),
        serial: cfg.device_serial.clone().unwrap_or_default(),
        device_name: state.system_settings.get_device_name().0,
        color: match state.system_settings.get_prime_color() {
            settings::global::SystemTheme::Dark => String::from("midnightbronze"),
            settings::global::SystemTheme::Light => String::from("arcticcopper"),
        },
        paths,
    };

    serde_json::to_string(&envoy_data).map_err(|e| anyhow::anyhow!("Could not serialize envoy json: {:?}", e))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GenericPathFormat {
    deriv: String,
    xpub: String,
    xfp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    first: Option<String>,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    _pub: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GenericFormat {
    chain: String, // BTC or TBTC
    // TODO: determine necessity of root xpub
    // xpub: String,
    xfp: String,
    account: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    fw_version: Option<String>,
    /// Identifies the exporting device (e.g. "passport-prime", "passport-core").
    /// Only emitted by connectors that opt in; importers may use it to label the wallet.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(flatten)]
    paths: BTreeMap<String, GenericPathFormat>,
}

pub fn generic_format(
    state: &AppState,
    id: &AccountId,
    cfg: &NgAccountConfig,
    export_fw_version: bool,
) -> Result<String, anyhow::Error> {
    generic_format_with_model(state, id, cfg, export_fw_version, None)
}

pub fn generic_format_with_model(
    state: &AppState,
    id: &AccountId,
    cfg: &NgAccountConfig,
    export_fw_version: bool,
    model: Option<&str>,
) -> Result<String, anyhow::Error> {
    let network_int = network_to_u32(cfg.network);

    let xfp = match id.fingerprint() {
        Some(f) => f.to_string(),
        None => anyhow::bail!("Could not get fingerprint for account id: {}", id),
    };

    let paths = cfg
        .descriptors
        .iter()
        .map(|d| {
            let addr_type = d.export_addr_hint.unwrap_or(d.address_type);
            let (bip_num, script_type) = bip_from_addr_type(&addr_type);

            let script_path = match script_type {
                Some(n) => format!("/{}'", n),
                None => String::new(),
            };

            let path = GenericPathFormat {
                deriv: format!("m/{}'/{}'/{}'{}", bip_num, network_int, cfg.index, script_path),
                xpub: extract_xpub_from_descriptor(&d.external.clone().unwrap_or_default()),
                xfp: xfp.clone(),
                first: None, // TODO
                name: name_from_addr_type(&addr_type).into(),
                _pub: None, // TODO
            };

            let script_note = match script_type {
                Some(n) => format!("_{}", n),
                None => String::new(),
            };

            let name = format!("bip{}{}", bip_num, script_note);
            (name, path)
        })
        .collect::<BTreeMap<String, GenericPathFormat>>();

    let chain = match cfg.network {
        NgNetwork::Bitcoin => String::from("BTC"),
        _ => String::from("TBTC"),
    };

    let generic_data = GenericFormat {
        chain,
        xfp,
        account: cfg.index,
        fw_version: if export_fw_version { Some(get_version_info(state)) } else { None },
        model: model.map(String::from),
        paths,
    };

    serde_json::to_string(&generic_data)
        .map_err(|e| anyhow::anyhow!("Could not serialize generic json: {:?}", e))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GenericMultiFormat {
    xfp: String,
    #[serde(flatten)]
    paths: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fw_version: Option<String>,
}

pub fn generic_multi_format(
    state: &AppState,
    id: &AccountId,
    cfg: &NgAccountConfig,
    export_fw_version: bool,
) -> Result<String, anyhow::Error> {
    let network_int = network_to_u32(cfg.network);

    let xfp = match id.fingerprint() {
        Some(f) => f.to_string(),
        None => anyhow::bail!("Could not get fingerprint for account id: {}", id),
    };

    let paths = cfg
        .descriptors
        .iter()
        .flat_map(|d| {
            let addr_type = d.export_addr_hint.unwrap_or(d.address_type);
            let (bip_num, script_type) = bip_from_addr_type(&addr_type);

            let script_path = match script_type {
                Some(n) => format!("/{}'", n),
                None => String::new(),
            };

            if bip_num != 48 {
                return Vec::new();
            }

            let xpub = extract_xpub_from_descriptor(&d.external.clone().unwrap_or_default());
            let deriv = format!("m/{}'/{}'/{}'{}", bip_num, network_int, cfg.index, script_path);

            let xpub_name = String::from(name_from_addr_type_swapped(&addr_type).replace("-", "_"));
            let deriv_name = format!("{}_deriv", xpub_name);

            vec![(deriv_name, deriv), (xpub_name, xpub)]
        })
        .collect::<BTreeMap<String, String>>();

    let generic_multi_data = GenericMultiFormat {
        xfp,
        paths,
        fw_version: if export_fw_version { Some(get_version_info(state)) } else { None },
    };

    serde_json::to_string(&generic_multi_data)
        .map_err(|e| anyhow::anyhow!("Could not serialize generic multi json: {:?}", e))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ElectrumKeystoreFormat {
    ckcc_xfp: u32,
    ckcc_xpub: String,
    hw_type: String,
    #[serde(rename = "type")]
    w_type: String,
    label: String,
    derivation: String,
    xpub: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ElectrumFormat {
    #[serde(skip_serializing_if = "Option::is_none")]
    seed_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    use_encryption: Option<bool>,
    wallet_type: String,
    keystore: ElectrumKeystoreFormat,
}

pub fn electrum_format(
    id: &AccountId,
    cfg: &NgAccountConfig,
    watch_only: bool,
) -> Result<String, anyhow::Error> {
    let network_int = network_to_u32(cfg.network);

    let fpr = match id.fingerprint() {
        Some(f) => f.to_string(),
        None => anyhow::bail!("Could not get fingerprint for account id: {}", id),
    };

    let xfp = u32::from_str_radix(fpr.as_str(), 16).unwrap_or(0).swap_bytes();

    let keystore = cfg
        .descriptors
        .iter()
        .filter_map(|d| {
            let addr_type = d.export_addr_hint.unwrap_or(d.address_type);
            let (bip_num, _) = bip_from_addr_type(&addr_type);

            if bip_num != 84 {
                return None;
            }

            let classic_xpub = extract_xpub_from_descriptor(&d.external.clone().unwrap_or_default());
            let zpub = convert_to_slip132_xpub(&classic_xpub, cfg.network, &addr_type)
                .unwrap_or(classic_xpub.clone());

            let path = ElectrumKeystoreFormat {
                ckcc_xfp: xfp,
                ckcc_xpub: classic_xpub,
                hw_type: String::from("passport"),
                w_type: if watch_only { String::from("bip32") } else { String::from("hardware") },
                label: format!("Passport Acct. {} ({})", cfg.index, fpr),
                derivation: format!("m/{}'/{}'/{}'", bip_num, network_int, cfg.index),
                xpub: zpub,
            };

            Some(path)
        })
        .next()
        .ok_or(anyhow::anyhow!("No segwit paths for Electrum export format in {}", id))?;

    let electrum_data = ElectrumFormat {
        seed_version: if watch_only { None } else { Some(17) },
        use_encryption: if watch_only { None } else { Some(false) },
        wallet_type: String::from("standard"),
        keystore,
    };

    serde_json::to_string(&electrum_data)
        .map_err(|e| anyhow::anyhow!("Could not serialize electrum json: {:?}", e))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VaultFormat {
    #[serde(rename = "ExtPubKey")]
    xpub: String,
    #[serde(rename = "MasterFingerprint")]
    xfp: String,
    #[serde(rename = "AccountKeyPath")]
    derivation: String,
    #[serde(rename = "FirmwareVersion")]
    fw_version: String,
    #[serde(rename = "Source")]
    source: String,
}

pub fn vault_format(
    state: &AppState,
    id: &AccountId,
    cfg: &NgAccountConfig,
) -> Result<String, anyhow::Error> {
    let network_int = network_to_u32(cfg.network);

    let xfp = match id.fingerprint() {
        Some(f) => f.to_string(),
        None => anyhow::bail!("Could not get fingerprint for account id: {}", id),
    };

    let vault_data = cfg
        .descriptors
        .iter()
        .filter_map(|d| {
            let addr_type = d.export_addr_hint.unwrap_or(d.address_type);
            let (bip_num, _) = bip_from_addr_type(&addr_type);

            if bip_num != 84 {
                return None;
            }

            let xpub = extract_xpub_from_descriptor(&d.external.clone().unwrap_or_default());

            let path = VaultFormat {
                xpub,
                xfp: xfp.clone(),
                derivation: format!("{}'/{}'/{}'", bip_num, network_int, cfg.index),
                fw_version: get_version_info(state),
                source: String::from("Passport"),
            };

            Some(path)
        })
        .next()
        .ok_or(anyhow::anyhow!("No segwit paths for Vault export format in {}", id))?;

    serde_json::to_string(&vault_data).map_err(|e| anyhow::anyhow!("Could not serialize Vault json: {:?}", e))
}

// #[derive(Debug, Serialize, Deserialize, Clone)]
// pub struct BitcoinCorePathFormat {
//     desc: String,
//     range: Vec<u32>,
//     timestamp: String,
//     internal: bool,
//     keypool: bool,
//     watchonly: bool,
// }

// pub fn bitcoin_core_format(id: &AccountId, cfg: &NgAccountConfig) -> Result<String, anyhow::Error> {
//     let xfp = match id.fingerprint() {
//         Some(f) => f.to_string(),
//         None => anyhow::bail!("Could not get fingerprint for account id: {}", id),
//     }
//     .to_uppercase();
//
//     let nb = format!("{:?}", cfg.network);
//
//     let payload_data = cfg
//         .descriptors
//         .iter()
//         .flat_map(|d| {
//             let addr_type = d.export_addr_hint.unwrap_or(d.address_type);
//             let (bip_num, _) = bip_from_addr_type(&addr_type);
//
//             if bip_num != 84 {
//                 return Vec::new();
//             }
//
//             let path_internal = BitcoinCorePathFormat {
//                 desc: d.internal.clone().replace("'", "h"),
//                 range: vec![0, 1000],
//                 timestamp: String::from("now"),
//                 internal: true,
//                 keypool: true,
//                 watchonly: true,
//             };
//
//             let path_external = BitcoinCorePathFormat {
//                 desc: d.external.clone().unwrap_or_default().replace("'", "h"),
//                 range: vec![0, 1000],
//                 timestamp: String::from("now"),
//                 internal: false,
//                 keypool: true,
//                 watchonly: true,
//             };
//
//             vec![path_internal, path_external]
//         })
//         .collect::<Vec<BitcoinCorePathFormat>>();
//
//     let payload = serde_json::to_string(&payload_data)
//         .map_err(|e| anyhow::anyhow!("Could not serialize bitcoin core json: {:?}", e))?;
//
//     Ok(format!(
//         "\
// # Bitcoin Core Wallet Import File
//
// ## For wallet with master key fingerprint: {xfp}
//
// Wallet operates on blockchain: {nb}
//
// ## Bitcoin Core RPC
//
// The following command can be entered after opening Window -> Console
// in Bitcoin Core, or using bitcoin-cli:
//
// importmulti '{payload}'"
//     ))
// }
