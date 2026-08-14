// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod protocol;

use {
    crate::{
        account_id::AccountId, gui_permissions::GuiPermissions, state::AppState, CasaHealth, CasaHealthState,
        FileSystem, Navigate,
    },
    anyhow::{bail, Context},
    fs::{Error as FsError, Location, OpenFlags},
    ngwallet::{bdk_wallet::bitcoin::Network, bip39::MasterKey},
    slint_keyos_platform::{
        gui_server_api::navigation::{
            filepicker::{AllowedLocations, Location as PickerLocation, SelectFileOptions},
            qrscanner::{ScanQrOptions, ScanQrResult},
        },
        navigation::{open_qr_scanner, select_file},
        slint::{ComponentHandle, ToSharedString},
        StoredValue,
    },
    std::{io::Read, path::Path},
    zeroize::Zeroize,
};

const MAX_HEALTH_CHECK_BYTES: usize = 64 * 1024;

pub fn init(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let global = ui.global::<CasaHealth>();

    global.on_start(move |id| {
        reset(state);
        state.borrow().ui().global::<Navigate>().invoke_casa_health(Default::default(), Default::default());
        let result = (|| {
            let id = id.as_str().parse::<AccountId>().context("invalid account ID")?;
            ensure_casa_account(state, &id)?;
            let ui = state.borrow().ui();
            ui.global::<CasaHealth>().set_account_id(id.to_string().into());
            Ok(())
        })();
        if let Err(error) = result {
            report_error(state, "CASA-HC-ACCOUNT", "start", &error);
        }
    });

    global.on_check_with_qr(move || {
        if let Err(error) = check_with_qr(state) {
            report_error(state, "CASA-HC-QR", "qr", &error);
        }
    });

    global.on_check_with_file(move || {
        if let Err(error) = check_with_file(state) {
            report_error(state, "CASA-HC-FILE", "file", &error);
        }
    });

    global.on_qr_parts(move |density| {
        state
            .borrow()
            .pending_casa_health_qr
            .as_ref()
            .map(|payload| slint_keyos_platform::qrcode::encode_qr_parts(protocol::UR_TYPE, payload, density))
            .unwrap_or_default()
    });

    global.on_done(move || {
        reset(state);
        state.borrow().ui().global::<Navigate>().invoke_backward();
    });
}

fn reset(state: StoredValue<AppState>) {
    if let Some(mut payload) = state.borrow_mut().pending_casa_health_qr.take() {
        payload.zeroize();
    }
    let ui = state.borrow().ui();
    let global = ui.global::<CasaHealth>();
    global.set_state(CasaHealthState::Idle);
    global.set_account_id("".into());
    global.set_saved_file_name("".into());
    global.set_error_code("".into());
}

fn ensure_casa_account(state: StoredValue<AppState>, id: &AccountId) -> anyhow::Result<()> {
    let state = state.borrow();
    if !id.is_multi() || !state.store.is_casa_account(id) {
        bail!("health checks are only available for Casa multisigs");
    }
    let config = state.store.get_account_config(id).context("account not found")?;
    config.multisig.as_ref().context("Casa account is not multisig")?;
    Ok(())
}

fn account_key(
    state: StoredValue<AppState>,
) -> anyhow::Result<(Network, MasterKey, ngwallet::bdk_wallet::bitcoin::bip32::DerivationPath)> {
    let id_text = state.borrow().ui().global::<CasaHealth>().get_account_id();
    let id = id_text.as_str().parse::<AccountId>().context("invalid Casa account ID")?;
    ensure_casa_account(state, &id)?;
    let state = state.borrow();
    let config = state.store.get_account_config(&id).context("account not found")?;
    let network = config.network;
    let master = state.store.load_master_key(network).context("load active Master Key")?;
    let signer = config
        .multisig
        .as_ref()
        .context("Casa account is not multisig")?
        .get_signers()
        .iter()
        .find(|signer| signer.get_fingerprint() == master.fingerprint)
        .context("Casa multisig does not belong to the active Master Key")?;
    let origin = signer.get_derivation().context("invalid Casa signer derivation")?;
    Ok((network, master, origin))
}

fn sign_challenge(state: StoredValue<AppState>, input: &[u8]) -> anyhow::Result<protocol::SignedResponse> {
    let (network, master, origin) = account_key(state)?;
    protocol::sign(&state.borrow().store.secp, &master, network, input, &[origin]).map_err(anyhow::Error::new)
}

fn check_with_qr(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let scan = open_qr_scanner::<GuiPermissions>(ScanQrOptions {
        // TODO: localization
        header_title: "Scan Casa Health Check".to_owned(),
        header_left_icon: "chevron-left".to_owned(),
        ..Default::default()
    })
    .context("open QR scanner")?;
    let Some(scan) = scan else { return Ok(()) };
    let (ur_type, mut cbor) = match scan {
        ScanQrResult::Ur2 { ur_type, data, .. } => (ur_type, data),
        ScanQrResult::LeftClicked | ScanQrResult::RightClicked | ScanQrResult::ButtonClicked => return Ok(()),
        ScanQrResult::Qr { .. } => bail!("Casa health check must be a UR bytes QR"),
    };
    if cbor.len() > MAX_HEALTH_CHECK_BYTES {
        cbor.zeroize();
        bail!("Casa health check is too large");
    }
    state.borrow().ui().global::<CasaHealth>().set_state(CasaHealthState::Working);
    let decoded = protocol::decode_ur(&ur_type, &cbor).map_err(anyhow::Error::new);
    cbor.zeroize();
    let mut challenge = decoded?;
    let signed = sign_challenge(state, &challenge);
    challenge.zeroize();
    let signed = signed?;
    let payload = protocol::encode_ur(&signed).map_err(anyhow::Error::new)?;
    state.borrow_mut().pending_casa_health_qr = Some(payload);
    state.borrow().ui().global::<CasaHealth>().set_state(CasaHealthState::QrReady);
    log::info!(target: "casa::health", "operation=health_check_qr stage=complete");
    Ok(())
}

fn check_with_file(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let options = SelectFileOptions::default()
        .with_start_location(PickerLocation::External)
        .with_allowed_locations(AllowedLocations::specific([
            PickerLocation::External,
            PickerLocation::Airlock,
        ]))
        .with_dirs_allowed(true)
        .with_multiple_selection_mode(false);
    let selection = select_file::<GuiPermissions>(options).context("open file picker")?;
    let Some(selection) = selection else { return Ok(()) };
    let Some((path, picker_location)) = selection.files().first() else {
        bail!("no Casa health-check file selected");
    };
    let location = match picker_location {
        PickerLocation::External => Location::Usb,
        PickerLocation::Airlock => Location::Airlock,
        PickerLocation::Internal => bail!("Casa health check must be read from microSD or Airlock"),
    };
    let source_path = Path::new(path);
    let source_name =
        source_path.file_name().and_then(|name| name.to_str()).context("invalid health-check filename")?;
    if !is_health_check_filename(source_name) {
        bail!("select a Casa -hc file that has not already been signed");
    }

    state.borrow().ui().global::<CasaHealth>().set_state(CasaHealthState::Working);
    let fs = FileSystem::default();
    let mut file = fs.open_file(path, location, OpenFlags::READ_ONLY).context("open health-check file")?;
    let mut challenge = Vec::new();
    file.by_ref()
        .take((MAX_HEALTH_CHECK_BYTES + 1) as u64)
        .read_to_end(&mut challenge)
        .context("read health-check file")?;
    if challenge.len() > MAX_HEALTH_CHECK_BYTES {
        challenge.zeroize();
        bail!("Casa health check is too large");
    }
    let signed = sign_challenge(state, &challenge);
    challenge.zeroize();
    let signed = signed?;

    let desired_name = signed_filename(source_name)?;
    let directory = source_path.parent().unwrap_or_else(|| Path::new(""));
    let (output_path, display_name) = collision_safe_output(&fs, location, directory, &desired_name)?;
    let mut output = fs.open_file(&output_path, location, OpenFlags::CREATE).context("create output file")?;
    let mut payload = signed.into_bytes();
    let write_result = output.overwrite(&payload).context("write signed health check");
    payload.zeroize();
    write_result?;

    let ui = state.borrow().ui();
    let global = ui.global::<CasaHealth>();
    global.set_saved_file_name(display_name.to_shared_string());
    global.set_state(CasaHealthState::FileSaved);
    log::info!(target: "casa::health", "operation=health_check_file stage=complete");
    Ok(())
}

fn is_health_check_filename(filename: &str) -> bool {
    let filename = filename.to_ascii_lowercase();
    filename.contains("-hc") && !filename.contains("-signed")
}

fn signed_filename(filename: &str) -> anyhow::Result<String> {
    let path = Path::new(filename);
    let stem = path.file_stem().and_then(|value| value.to_str()).context("invalid filename stem")?;
    let extension =
        path.extension().and_then(|value| value.to_str()).context("missing filename extension")?;
    Ok(format!("{stem}-signed.{extension}"))
}

fn collision_safe_output(
    fs: &FileSystem,
    location: Location,
    directory: &Path,
    desired_name: &str,
) -> anyhow::Result<(String, String)> {
    let desired_path =
        directory.join(desired_name).to_str().map(str::to_owned).context("invalid output path")?;
    match fs.metadata(&desired_path, location) {
        Err(FsError::FileNotFound) => return Ok((desired_path, desired_name.to_owned())),
        Ok(_) => {}
        Err(error) => return Err(error).context("check output filename"),
    }
    let directory_path = directory.to_str().context("invalid output directory")?;
    let output_dir = fs.open_dir(directory_path, location).context("open output directory")?;
    let name = output_dir.pick_next_filename(desired_name, None).context("choose output filename")?;
    let path = directory.join(&name).to_str().map(str::to_owned).context("invalid output path")?;
    Ok((path, name))
}

fn report_error(
    state: StoredValue<AppState>,
    code: &'static str,
    stage: &'static str,
    error: &anyhow::Error,
) {
    let protocol_code = error.downcast_ref::<protocol::Error>().map(|error| error.code()).unwrap_or(code);
    log::error!(target: "casa::health", "operation=health_check stage={stage} code={protocol_code}");
    let ui = state.borrow().ui();
    let global = ui.global::<CasaHealth>();
    global.set_error_code(protocol_code.into());
    global.set_state(CasaHealthState::Error);
}
