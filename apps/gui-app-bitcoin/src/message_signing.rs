// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        account_id::AccountId, gui_permissions::GuiPermissions, state::AppState, tr, Animate, AppWindow,
        FileSaveState, MessageSignatureView, Navigate, NavigateOptions, RouteOption, RouteState, SignMessage,
        SignMessageState, TrId,
    },
    anyhow::{anyhow, bail, Context},
    ngwallet::{
        bdk_wallet::bitcoin::{
            base64::{prelude::BASE64_STANDARD, Engine},
            bip32::{DerivationPath, Xpriv},
            key::TapTweak,
            secp256k1::{All, Message, Secp256k1, XOnlyPublicKey},
            sign_message::{signed_msg_hash, MessageSignature},
            Address, CompressedPublicKey, Network as BdkNetwork, NetworkKind, PrivateKey, PublicKey,
        },
        bip32::NgAccountPath,
    },
    slint_keyos_platform::{
        gui_server_api::navigation::qrscanner::{ScanQrOptions, ScanQrResult},
        navigation::open_qr_scanner,
        slint::{ComponentHandle, ToSharedString},
        spawn_local, StoredValue,
    },
    std::{io::Write, str::FromStr, thread, time::Duration},
};

const SIGNMESSAGE_PREFIX: &str = "signmessage";
const ASCII_PREFIX: &str = "ascii:";
const BASE_FILENAME: &str = "signed-message.txt";
const SIGNED_MESSAGES_DIR: &str = "signed_messages/";

fn set_error_and_navigate(ui: &AppWindow, global: &SignMessage) {
    global.set_state(SignMessageState::Error);
    if ui.global::<RouteState>().get_active() != RouteOption::SignMessage {
        ui.global::<Navigate>()
            .invoke_sign_message(NavigateOptions { replace: false, animate: Animate::None });
    }
}

fn validate_derivation_path_and_account(
    state: &StoredValue<AppState>,
    derivation_path_str: &str,
    account_id_str: &str,
) -> anyhow::Result<(DerivationPath, NgAccountPath, BdkNetwork)> {
    let derivation_path = DerivationPath::from_str(derivation_path_str).context("Invalid derivation path")?;

    let account_path = NgAccountPath::parse(&derivation_path)
        .context("Failed to parse account path")?
        .context("Unsupported derivation path format")?;

    let parsed_account_id = account_id_str.parse::<AccountId>().context("Invalid account ID")?;
    let state_borrow = state.borrow();
    let account_config =
        state_borrow.store.get_account_config(&parsed_account_id).context("Account not found")?;

    let path_network_kind =
        account_path.to_network_kind().context("Failed to determine network from derivation path")?;
    let account_network_kind = match account_config.network {
        BdkNetwork::Bitcoin => NetworkKind::Main,
        _ => NetworkKind::Test,
    };
    if path_network_kind != account_network_kind {
        bail!("Network mismatch: path {:?}, account {:?}", path_network_kind, account_network_kind);
    }

    if account_path.account != account_config.index {
        bail!("Account index mismatch: path {}, account {}", account_path.account, account_config.index);
    }

    Ok((derivation_path, account_path, account_config.network))
}

fn derive_key_and_address(
    state: &StoredValue<AppState>,
    derivation_path: &DerivationPath,
    network: BdkNetwork,
    purpose: u32,
) -> anyhow::Result<(PrivateKey, Address)> {
    let state_borrow = state.borrow();
    let master_key = state_borrow.store.load_master_key(network).context("Failed to load master key")?;
    let secp = &state_borrow.store.secp;

    let xpriv = Xpriv::new_master(network, &master_key.key.0)
        .context("Failed to create xpriv from master key")?
        .derive_priv(secp, derivation_path)
        .context("Failed to derive private key")?;

    let private_key = PrivateKey::new(xpriv.private_key, network);
    let public_key = private_key.public_key(secp);
    let compressed_pubkey =
        CompressedPublicKey::try_from(public_key).context("Failed to compress public key")?;

    let address = derive_address_from_purpose(purpose, &compressed_pubkey, &public_key, network, secp)?;

    Ok((private_key, address))
}

pub fn init(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let global = ui.global::<SignMessage>();

    global.on_start_sign_message(move |account_id| match start_sign_message(state, account_id.to_string()) {
        Ok(_) => {}
        Err(e) => {
            log::error!("failed to start sign message {e:?}");
            let ui = state.borrow().ui();
            let global = ui.global::<SignMessage>();
            set_error_and_navigate(&ui, &global);
        }
    });

    global.on_cancel_signing(move || {
        let ui = state.borrow().ui();
        ui.global::<SignMessage>().set_state(SignMessageState::Idle);
    });

    global.on_sign_message(move || {
        if let Err(e) = sign_message(state) {
            log::error!("failed to sign message {e:?}");
            let ui = state.borrow().ui();
            let global = ui.global::<SignMessage>();
            global.set_state(SignMessageState::Error);
        }
    });

    global.on_get_signed_message_formatted(move || {
        let ui = state.borrow().ui();
        let global = ui.global::<SignMessage>();
        let sig = global.get_signed_message();

        format!(
            "-----BEGIN BITCOIN SIGNED MESSAGE-----\n{}\n-----BEGIN SIGNATURE-----\n{}\n{}\n-----END BITCOIN SIGNED MESSAGE-----",
            sig.message, sig.address, sig.signature
        )
        .to_shared_string()
    });

    global.on_save_signed_message_to_file(move || {
        spawn_local(async move {
            match save_signed_message_to_file(state).await {
                Ok(_) => {}
                Err(e) => {
                    log::error!("failed to save signed message {e:?}");
                    let ui = state.borrow().ui();
                    let global = ui.global::<SignMessage>();
                    global.set_file_save_state(FileSaveState::Error);
                }
            }
        })
        .detach();
    });
}

fn start_sign_message(state: StoredValue<AppState>, account_id: String) -> anyhow::Result<()> {
    let opts = ScanQrOptions {
        header_title: tr::lookup_id(TrId::SignMessageTitle).into(),
        message: String::new(),
        header_left_icon: String::new(),
        header_right_icon: String::from("close"),
        button_icon: String::from("file"),
        button_text: tr::lookup_id(TrId::SignMessageSignWithFile).into(),
        ..Default::default()
    };

    let scan = match open_qr_scanner::<GuiPermissions>(opts) {
        Ok(Some(s)) => s,
        Ok(None) => {
            log::info!("qr scanner returned no data");
            return Ok(());
        }
        Err(e) => {
            log::info!("failed to open qr scanner: {e:?}");
            return Ok(());
        }
    };

    let string = match scan {
        ScanQrResult::Qr { data, .. } => String::from_utf8(data).context("invalid qr utf8")?,
        ScanQrResult::ButtonClicked => {
            // sleep to avoid the OOM killer closing the bitcoin app
            thread::sleep(Duration::from_millis(500));

            let bytes = crate::callbacks::execute_file_picker(state)?;

            match bytes {
                Some(b) => String::from_utf8(b).context("invalid file utf8")?,
                None => return Ok(()),
            }
        }
        ScanQrResult::RightClicked => return Ok(()),
        ScanQrResult::Ur2 { .. } => {
            bail!("UR2 not supported for message signing");
        }
        action => {
            bail!("unexpected scan result: {:?}", action);
        }
    };

    let ui = state.borrow().ui();
    let global = ui.global::<SignMessage>();

    let parts: Vec<&str> = string.splitn(3, ' ').collect();

    let (derivation_path_str, message) = if parts.len() == 3 && parts[0] == SIGNMESSAGE_PREFIX {
        let path = parts[1].to_string();
        let msg =
            parts[2].strip_prefix(ASCII_PREFIX).context("message must start with 'ascii:'")?.to_string();
        (path, msg)
    } else {
        let lines: Vec<&str> = string.lines().collect();
        if lines.len() != 3 {
            bail!("Invalid format: expected 3 lines, got {}", lines.len());
        }
        let msg = lines[0].to_string();
        let path = lines[1].to_string();
        (path, msg)
    };

    let (derivation_path, account_path, network) =
        validate_derivation_path_and_account(&state, &derivation_path_str, &account_id)?;

    match account_path.purpose {
        44 | 49 | 84 | 86 | 48 => {}
        _ => {
            bail!("Unsupported purpose: {}", account_path.purpose);
        }
    }

    let (_private_key, address) =
        derive_key_and_address(&state, &derivation_path, network, account_path.purpose)?;

    global.set_derivation_path(derivation_path_str.to_shared_string());
    global.set_address(address.to_string().to_shared_string());
    global.set_message(message.to_shared_string());
    global.set_account_id(account_id.to_shared_string());
    global.set_state(SignMessageState::Sign);

    if ui.global::<RouteState>().get_active() != RouteOption::SignMessage {
        ui.global::<Navigate>()
            .invoke_sign_message(NavigateOptions { replace: false, animate: Animate::None });
    }

    Ok(())
}

fn sign_message(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let ui = state.borrow().ui();
    let global = ui.global::<SignMessage>();

    global.set_state(SignMessageState::Signing);

    let derivation_path_str = global.get_derivation_path().to_string();
    let message = global.get_message().to_string();
    let account_id_str = global.get_account_id().to_string();

    let (_derivation_path, account_path, network) =
        validate_derivation_path_and_account(&state, &derivation_path_str, &account_id_str)?;

    let (private_key, address) =
        derive_key_and_address(&state, &_derivation_path, network, account_path.purpose)?;

    let secp = &state.borrow().store.secp;
    let msg_hash = signed_msg_hash(&message);
    let msg = Message::from_digest_slice(msg_hash.as_ref()).map_err(|e| anyhow!("invalid digest: {e:?}"))?;
    let signature = secp.sign_ecdsa_recoverable(&msg, &private_key.inner);

    let message_signature = MessageSignature { signature, compressed: true };
    let sig_bytes = message_signature.serialize();
    let sig_base64 = BASE64_STANDARD.encode(sig_bytes);

    let signed_message = MessageSignatureView {
        message: message.to_shared_string(),
        address: address.to_string().to_shared_string(),
        signature: sig_base64.to_shared_string(),
    };

    global.set_signed_message(signed_message);
    global.set_state(SignMessageState::Success);

    Ok(())
}

async fn save_signed_message_to_file(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let ui = state.borrow().ui();
    let global = ui.global::<SignMessage>();

    let formatted = global.invoke_get_signed_message_formatted().to_string();

    let app_state = state.borrow();

    let signed_messages_dir = app_state
        .store
        .fs
        .create_dir(SIGNED_MESSAGES_DIR, fs::Location::Airlock)
        .context("Could not create signed_messages directory")?;

    let filename =
        signed_messages_dir.pick_next_filename(BASE_FILENAME, None).context("Could not get filename")?;

    let path = format!("{}{}", SIGNED_MESSAGES_DIR, filename);

    let mut file = app_state
        .store
        .fs
        .open_file(&path, fs::Location::Airlock, fs::OpenFlags { read: false, write: true, create: true })
        .context("Failed to create file")?;

    file.write_all(formatted.as_bytes()).context("Failed to write to file")?;

    global.set_saved_file_path(path.to_shared_string());
    global.set_file_save_state(FileSaveState::Saved);

    Ok(())
}

fn derive_address_from_purpose(
    purpose: u32,
    compressed_pubkey: &CompressedPublicKey,
    public_key: &PublicKey,
    network: BdkNetwork,
    secp: &Secp256k1<All>,
) -> anyhow::Result<Address> {
    match purpose {
        44 | 48 => Ok(Address::p2pkh(public_key, network)),
        49 => Ok(Address::p2shwpkh(compressed_pubkey, network)),
        84 => Ok(Address::p2wpkh(compressed_pubkey, network)),
        86 => {
            let x_only_pubkey = XOnlyPublicKey::from(public_key.inner);
            let (tweaked_key, _parity) = x_only_pubkey.tap_tweak(secp, None);
            Ok(Address::p2tr_tweaked(tweaked_key, network))
        }
        _ => bail!("Unsupported purpose: {}", purpose),
    }
}
