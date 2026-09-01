// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![feature(lazy_get)]
#![feature(must_not_suspend)]
#![deny(must_not_suspend)]

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use backup::messages::RestoreProgress;
use ngwallet::{account::RemoteUpdate, bdk_wallet::bitcoin::Network};
use quantum_link::{
    foundation_api::{
        firmware::{FirmwareInstallEvent, InstallErrorStage},
        onboarding::OnboardingState,
    },
    messages::*,
    PairingEvent,
};
use security::{messages::RawPin, MasterKeyState, PinEntryMode};
use slint_keyos_platform::{
    app, async_archive,
    futures_lite::StreamExt,
    gui_server_api::{
        navigation::filepicker::{AllowedExtensions, AllowedLocations, Location, SelectFileOptions},
        InputMessage,
    },
    navigation, quit_runtime,
    router::Router,
    settings::global::{OnboardingStatus, SystemTheme},
    sleep,
    slint::{ComponentHandle, ModelRc, SharedString, ToSharedString},
    spawn_local, spawn_worker, subscribe_archive, timeout, StoredValue,
};
use update::messages::ProgressUpdate;

use crate::{
    power_manager_ext_permissions::PowerManagerExtPermissions,
    quantum_link_permissions::QuantumLinkPermissions,
    security_permissions::SecurityPermissions,
    state::{erase::init_erase_callbacks, AppState},
    update_permissions::UpdatePermissions,
};

#[cfg(not(feature = "production"))]
mod debug;
mod state;

app_manager::use_api!();
backup::use_api!();
bt::use_api!();
haptics::use_api!();
keycard::use_api!();
power_manager::use_api!();
power_manager::use_ext_api!();
quantum_link::use_api!();
quantum_link::use_prestart_api!();
security::use_api!();
update::use_api!();

app!("Onboarding", kind = Onboarding);

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    cx.config.enable_swipe_back.set(false);

    let state = init_state(ui.clone_strong(), cx.gui.clone());

    ql_utils::on_ble_address(state.borrow().bluetooth.clone(), move |addr| {
        log::info!("Got BLE address: {addr:?}");
        state.borrow_mut().bt_address = addr;
    });

    #[cfg(not(feature = "production"))]
    debug::init(state);

    init_device_name(state);
    init_router_middleware(cx.router, state);
    init_callbacks(state);
    init_connect_wallet(state);
    init_backup(state);
    init_seed_global(state);
    init_quantum_link(state);
    init_erase_callbacks(state);

    on_startup(state);
    init_ql_status_monitor(cx.router, state);

    cx.set_input_handler(move |input| match input.msg {
        InputMessage::Hidden if state.borrow().finished => {
            // Wait for actually being hidden before exiting, because exiting immediately
            // throws away the framebuffer that the gui-server might still be displaying.
            quit_runtime();
        }
        InputMessage::Custom1 => {
            let ui = state.borrow().ui();
            let cb = ui.global::<OnboardingCallbacks>();
            let seed = ui.global::<SeedGlobal>();
            if !cb.get_fatal_disconnect() && !seed.get_is_master_key_recovery() {
                cb.set_show_restart_modal(true);
            }
        }
        _ => {}
    });

    ui.run().unwrap();
}

fn init_state(ui: AppWindow, gui: Arc<GuiApi>) -> StoredValue<AppState> {
    let security = Security::default();

    if security.logged_in() {
        log::info!("User is logged in");
    } else {
        log::info!("PIN not set, starting onboarding from welcome screen");
        quantum_link::start_quantum_link_without_filesystem::<QuantumLinkPrestartPermissions>();
    }

    // Now that quantum link is actually started (with or without a filesystem),
    // we can connect to it.
    StoredValue::new(AppState::new(ui.as_weak(), gui, security))
}

fn on_startup(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();

    let nav = ui.global::<Navigate>();
    let cb = ui.global::<OnboardingCallbacks>();

    let master_key_state = state.borrow().security.master_key_state();

    if matches!(master_key_state, MasterKeyState::Erased) {
        log::info!("master key erased, prioritizing master key recovery flow");
        ui.global::<SeedGlobal>().set_is_master_key_recovery(true);
        nav.invoke_master_key_deleted_main(NavigateOptions { animate: Animate::None, replace: true });
        return;
    }

    if state.borrow().update.check_update_applied() {
        log::info!("update successfully completed");
        let installed_version = {
            let state = state.borrow();
            cb.set_fw_update_state(FwUpdateState::Completed);
            nav.invoke_update_progress(NavigateOptions { animate: Animate::None, replace: false });
            state.update.clear_update_applied();
            state.update.firmware_version().unwrap_or_default()
        };
        notify_update_progress(state, FirmwareInstallEvent::Success { installed_version });
    } else if state.borrow().update.update_status().needs_continue {
        log::info!("continuing update post-reboot");
        state.borrow().update.continue_update();
        nav.invoke_update_progress(NavigateOptions { animate: Animate::None, replace: false });
        notify_update_progress(state, FirmwareInstallEvent::Installing);
    } else {
        log::info!("no update state detected");
        match master_key_state {
            MasterKeyState::Onboarding => {}
            _ => {
                #[cfg(keyos)]
                {
                    // if we have reached this branch, then we have rebooted mid-way through onboarding
                    // which is un-recoverable. we must factory reset restart onboarding
                    crate::erase_system_state();
                    state.borrow().security.lockout(security::LockoutOptions::erase_all()).ok();
                }
            }
        }
    }
}

fn navigate_backup(state: StoredValue<AppState>) {
    let state = state.borrow();
    let ui = state.ui();
    let nav = ui.global::<Navigate>();
    // from magic, you can choose manual backup
    nav.invoke_magic_backup(Default::default());
}

fn init_device_name(state: StoredValue<AppState>) {
    use slint_keyos_platform::settings::global::DeviceName;

    let current_device_name = state.borrow().settings.get_device_name();
    if current_device_name.0 == DeviceName::DEFAULT {
        spawn_local(async move {
            let device_id = loop {
                let device_id_result = state.borrow().security.device_id();
                match device_id_result {
                    Ok(device_id) => break device_id,
                    Err(security::GetDeviceIdError::NoBluetoothSerialYet) => {
                        sleep(Duration::from_millis(500)).await;
                    }
                    Err(e) => {
                        log::error!("Error getting Device Id: {e:?}");
                        return;
                    }
                }
            };
            let new_name = format!("{} ({:02X}{:02X})", DeviceName::DEFAULT, device_id.0[0], device_id.0[1],);
            log::info!("Default device name found, setting it to {new_name}");
            state.borrow().settings.set_device_name(new_name);
        })
        .detach();
    }
}

fn init_router_middleware(router: StoredValue<Router>, state: StoredValue<AppState>) {
    let mut router = router.borrow_mut();

    // cancel outstanding tasks, if any
    router.register_on_navigation_end(move |_| {
        spawn_local(async move {
            state.borrow_mut().cancel_tasks();
        })
        .detach();
    });

    // notify envoy when we move to a new (relevant) page
    router.register_on_navigation_end(move |_history| {
        spawn_local(async move {
            let ui = state.borrow().ui();
            let route = ui.global::<RouteState>().get_active();

            let msg = match route {
                RouteOption::Welcome => None,
                RouteOption::BackupCreated => Some(OnboardingState::MagicBackupCreated),
                RouteOption::CreatingMagicBackup => Some(OnboardingState::CreatingMagicBackup),
                RouteOption::MagicBackup => Some(OnboardingState::MagicBackupScreen),
                RouteOption::ManualKeycardBackup => Some(OnboardingState::CreatingKeycardBackup),
                RouteOption::ManualBackup => Some(OnboardingState::CreatingManualBackup),
                RouteOption::ManualBackupSeed => Some(OnboardingState::WritingDownSeedWords),
                RouteOption::VerifySeedWords => Some(OnboardingState::WritingDownSeedWords),
                RouteOption::CheckEnvoy => None,
                RouteOption::ScanQr => None,
                RouteOption::ConnectWallet => Some(OnboardingState::ConnectingWallet),
                RouteOption::CreateMasterSeed => Some(OnboardingState::CreatingWallet),
                RouteOption::MasterSeed => Some(OnboardingState::WalletCreationScreen),
                RouteOption::EnterBackupCode => None,
                RouteOption::EnterBackupWords => None,
                RouteOption::ImportSettings => None,
                RouteOption::RestoreSeed => None,
                RouteOption::RestoreSeedQr => None,
                RouteOption::RestoreFileBackupRestoring => None,
                RouteOption::RestoreKeycardBackup => None,
                RouteOption::RestoreMagicBackup => None,
                RouteOption::RestoreSeedWords => None,
                RouteOption::SetPinInfo => Some(OnboardingState::SecuringDevice),
                RouteOption::SetPinSet => None,
                RouteOption::SetPinSuccess => Some(OnboardingState::DeviceSecured),
                RouteOption::TermsOfUse => None,
                RouteOption::UpdateDevice => Some(OnboardingState::FirmwareUpdateScreen),
                RouteOption::UpdateProgress => None,
                RouteOption::MasterKeyDeletedMain => None,
                RouteOption::MasterKeyDeletedErase => None,
                RouteOption::MasterKeyDeletedErasing => None,
                RouteOption::MasterKeyDeletedRestore => None,
                RouteOption::KeycardTip => None,
            };

            if let Some(msg) = msg {
                log::info!("notifying onboarding state page change {msg:?}");
                notify_onboarding_state(state, msg);
            }
        })
        .detach();
    });
}

fn init_callbacks(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let cb = ui.global::<OnboardingCallbacks>();

    let version = state.borrow().security.os_version_info().map_or_else(
        |_| "unknown".to_string(),
        |opt| {
            opt.map(|info| String::from_utf8_lossy(&info.keyos_version).to_string())
                .unwrap_or_else(|| "N/A".to_string())
        },
    );
    cb.set_current_keyos_version(SharedString::from(version));

    //
    // onboarding events
    //

    ql_utils::on_bt_state_change(state.borrow().ql_status.clone(), move |connected| {
        log::info!("bt state changed {connected}");
        let ui = state.borrow().ui();
        ui.global::<OnboardingCallbacks>().set_bt_connected(connected);
    })
    .detach();

    cb.on_set_dark_mode(move |dark_mode: bool| {
        let state = state.borrow();
        let theme = if dark_mode { SystemTheme::Dark } else { SystemTheme::Light };
        state.settings.set_system_theme(theme);
    });

    cb.on_qr_data(move || {
        let state = state.borrow();
        spawn_worker(async_archive::<QuantumLinkPermissions, _>(UnpairFromEnvoy)).detach();
        ql_utils::static_qr(&state.settings, &state.bt_address, false).into()
    });

    cb.on_animated_qr_data(move || {
        let state = state.borrow();
        ql_utils::animated_qr(&state.quantum)
    });

    cb.on_navigate_backup(move || {
        navigate_backup(state);
    });

    cb.on_set_magic_backup_enabled(move |enabled| {
        state.borrow().settings.set_magic_backup_enabled(enabled);
    });

    cb.on_download_firmware_update(move || {
        start_firmware_download(state);
    });

    fn select_and_copy_release_file() -> anyhow::Result<Option<String>> {
        let options = SelectFileOptions::default()
            .with_hidden_allowed(false)
            .with_search_allowed(false)
            .with_start_location(Location::External)
            .with_allowed_locations(AllowedLocations::specific([Location::External]))
            .with_allowed_extensions(AllowedExtensions::specific(["tar"]));

        let mut fs = FileSystem::default();
        if let Some(res) = navigation::select_file::<gui_permissions::GuiPermissions>(options)? {
            let Some((path, Location::External)) =
                res.files().first().map(|(file, location)| (file.to_owned(), *location))
            else {
                log::info!("No files were selected");
                return Ok(None);
            };

            let mut src = fs
                .open_file(path, fs::Location::Usb, fs::OpenFlags { read: true, write: false, create: false })
                .context("Failed to open source file")?;

            let update_temp_file = update_temp_file();
            fs.ensure_parent_dir_exists(&update_temp_file, fs::Location::System)
                .context("Failed to create destination parent dir")?;

            let mut dst = fs
                .open_file(
                    &update_temp_file,
                    fs::Location::System,
                    fs::OpenFlags { read: true, write: true, create: true },
                )
                .context("Failed to create destination file")?;

            std::io::copy(&mut src, &mut dst).context("Failed to copy file")?;
            fs.flush(fs::Location::System).context("Failed to flush fs")?;

            drop(src);
            drop(dst);

            return Ok(Some(update_temp_file));
        }

        Ok(None)
    }

    cb.on_update_from_file(move || {
        let Ok(file) =
            select_and_copy_release_file().inspect_err(|e| log::error!("Couldn't update from a file: {e:?}"))
        else {
            let ui = state.borrow().ui();
            let cb = ui.global::<OnboardingCallbacks>();
            cb.set_fw_update_state(crate::FwUpdateState::Failed);
            cb.set_fw_update_error(FwUpdateError::VerifyFailed);

            return;
        };

        if let Some(file) = file {
            state.borrow().update.start_update(vec![file]);
        }
    });

    ql_utils::on_update_sufficient_battery::<PowerManagerExtPermissions, _>(move |sufficient_battery| {
        log::info!("update sufficient_battery={}", sufficient_battery);
        let ui = state.borrow().ui();
        ui.global::<OnboardingCallbacks>().set_update_sufficient_battery(sufficient_battery);
    })
    .detach();
}

fn init_quantum_link(state: StoredValue<AppState>) {
    spawn_local(async move {
        let mut pairing_events =
            subscribe_archive::<QuantumLinkPermissions, _>(quantum_link::messages::SubscribePairingEvent);
        while let Some(pairing_event) = pairing_events.next().await {
            log::info!("Got pairing event: {pairing_event:?}");
            let ui = state.borrow().ui();
            let nav = ui.global::<Navigate>();
            match pairing_event {
                PairingEvent::RequestReceived => {}
                PairingEvent::PairingComplete { device_name: _, new } => {
                    if new {
                        log::info!("Pairing completed successfully");
                        nav.invoke_check_envoy(NavigateOptions { replace: true, ..Default::default() });
                    }
                }
                PairingEvent::PairingFailed => {}
                PairingEvent::Disconnected => {}
            }
        }
    })
    .detach();

    spawn_local(async move {
        let mut security_check = subscribe_archive::<QuantumLinkPermissions, _>(
            quantum_link::messages::SubscribeSecurityCheckState,
        );
        while let Some(security_state) = security_check.next().await {
            log::info!("Security check state update: {security_state:?}");
            let ui = state.borrow().ui();
            let callbacks = ui.global::<OnboardingCallbacks>();

            match security_state {
                quantum_link::SecurityCheckState::ReceivedChallenge => {
                    callbacks.set_security_state(crate::SecurityCheckState::Loading);
                }
                quantum_link::SecurityCheckState::Success => {
                    callbacks.set_security_state(crate::SecurityCheckState::Passed);
                }
                quantum_link::SecurityCheckState::Failed => {
                    callbacks.set_security_state(crate::SecurityCheckState::Failed);
                }
                quantum_link::SecurityCheckState::Error => {
                    callbacks.set_security_state(crate::SecurityCheckState::Error);
                }
            }
        }
    })
    .detach();

    spawn_local(async move {
        let mut update_events =
            subscribe_archive::<UpdatePermissions, _>(update::messages::SubscribeUpdateProgress);

        while let Some(event) = update_events.next().await {
            let ui = state.borrow().ui();
            let callbacks = ui.global::<OnboardingCallbacks>();

            match event {
                ProgressUpdate::DownloadProgress(progress) => {
                    callbacks.set_fw_update_state(FwUpdateState::Receiving);
                    callbacks.set_fw_update_progress(progress.completion_percentage() as f32);
                }
                ProgressUpdate::DownloadComplete => {
                    log::info!("update download complete");
                    notify_update_progress(state, FirmwareInstallEvent::Installing);
                    state.borrow().update.apply_downloaded_update();
                    callbacks.set_fw_update_state(FwUpdateState::Installing);
                }
                ProgressUpdate::InstallProgress(progress) => {
                    callbacks.set_fw_update_state(FwUpdateState::Installing);

                    let percent = progress.completion_percentage;
                    let secs_remaining = progress.estimated_seconds_remaining;
                    let mins_remaining = secs_remaining.div_ceil(60).max(1);
                    let time_str = format!("{mins_remaining}m");

                    log::info!("update install progress {percent}% {time_str}");

                    callbacks.set_fw_update_progress(percent as f32);
                    callbacks.set_fw_update_eta(time_str.into());
                }
                ProgressUpdate::Rebooting => {
                    log::info!("update rebooting");
                    callbacks.set_fw_update_state(crate::FwUpdateState::Restarting);
                    notify_update_progress(state, FirmwareInstallEvent::Rebooting);
                }
                ProgressUpdate::Done => {
                    log::info!("update complete. rebooting...");
                    FileSystem::default().remove(update_temp_file(), fs::Location::System).ok();

                    callbacks.set_fw_update_state(crate::FwUpdateState::Restarting);
                    notify_update_progress(state, FirmwareInstallEvent::Rebooting);
                }
                ProgressUpdate::InstallError(error) => {
                    log::error!("failed to apply update {error:?}");
                    FileSystem::default().remove(update_temp_file(), fs::Location::System).ok();

                    callbacks.set_fw_update_state(crate::FwUpdateState::Failed);
                    callbacks.set_fw_update_error(FwUpdateError::InstallFailed);
                    notify_update_progress(
                        state,
                        FirmwareInstallEvent::Error {
                            error: error.to_string(),
                            stage: InstallErrorStage::Install,
                        },
                    );
                }
                ProgressUpdate::DownloadError(error) => {
                    log::error!("failed to download update {error:?}");
                    callbacks.set_fw_update_state(crate::FwUpdateState::Failed);
                    callbacks.set_fw_update_error(FwUpdateError::DownloadFailed);
                    notify_update_progress(
                        state,
                        FirmwareInstallEvent::Error {
                            error: error.to_string(),
                            stage: InstallErrorStage::Download,
                        },
                    );
                }
            }
        }
    })
    .detach();

    spawn_local(async move {
        let mut restore_events = subscribe_archive::<backup_permissions::BackupPermissions, _>(
            backup::messages::SubscribeRestoreProgress,
        );
        while let Some(event) = restore_events.next().await {
            log::info!("Got restore progress event: {event:?}");
            let ui = state.borrow().ui();
            let global = ui.global::<SeedGlobal>();
            match event {
                RestoreProgress::NotFound => {
                    global.set_restore_backup_state(RestoreBackupState::NotFound);
                }
                RestoreProgress::Downloading => {
                    global.set_restore_backup_state(RestoreBackupState::Downloading);
                }
                RestoreProgress::Restoring => {
                    global.set_restore_backup_state(RestoreBackupState::Restoring);
                }
                RestoreProgress::Restored => {
                    // wait a while for system load to go down.
                    spawn_local(async move {
                        sleep(Duration::from_secs(2)).await;
                        let ui = state.borrow().ui();
                        let global = ui.global::<SeedGlobal>();
                        global.set_restore_backup_state(RestoreBackupState::Restored);
                    })
                    .detach()
                }
                RestoreProgress::Error => {
                    global.set_restore_backup_state(RestoreBackupState::Error);
                }
            }
        }
    })
    .detach();

    {
        let state = state.borrow();
        ql_utils::sync_system_timezone(state.settings.clone(), state.ql_status.clone(), |e| {
            log::info!("failed to retrieve envoy timezone, retrying... {e}");
        })
        .detach();
    }

    let ui = state.borrow().ui();

    ui.global::<OnboardingCallbacks>().on_check_firmware_update(move || {
        spawn_local(check_firmware_update_available(state)).detach();
    });
}

fn init_ql_status_monitor(router: StoredValue<Router>, state: StoredValue<AppState>) {
    match state.borrow().security.master_key_state() {
        MasterKeyState::Onboarding => {}
        _ => {
            return;
        }
    }

    start_ql_status_monitor(state);

    let ui = state.borrow().ui();
    let cb = ui.global::<OnboardingCallbacks>();
    cb.on_restart_onboarding(move || {
        spawn_local(async move {
            let key_state = state.borrow().security.master_key_state();
            match key_state {
                MasterKeyState::Onboarding => {
                    router.borrow_mut().clear_history();
                    let ui = state.borrow().ui();
                    let nav = ui.global::<Navigate>();
                    clear_slint_state(&ui, &state.borrow());
                    start_ql_status_monitor(state);
                    nav.invoke_welcome(NavigateOptions { animate: Animate::None, replace: false });
                }
                _ => {
                    log::info!("resetting prime to go through onboarding again");
                    erase_system_state();
                    async_archive::<SecurityPermissions, _>(security::messages::Lockout {
                        lockout_options: security::LockoutOptions::erase_all(),
                        reboot: true,
                    })
                    .await
                    .inspect_err(|_| log::error!("failed to factory reset on fatal disconnect"))
                    .ok();
                }
            }
        })
        .detach()
    });
}

fn start_ql_status_monitor(state: StoredValue<AppState>) {
    let task = spawn_local(async move {
        let ql_status = state.borrow().ql_status.clone();
        let ui = state.borrow().ui();
        let cb = ui.global::<OnboardingCallbacks>();
        loop {
            ql_status.ready().await;
            cb.set_fatal_disconnect(false);
            ql_status.wait_until(|s| !s.bt_connected || !s.ql_paired || !s.live).await;

            let error = futures_lite::future::or(
                async {
                    ql_status.ready().await;
                    false
                },
                async {
                    sleep(Duration::from_secs(30)).await;
                    true
                },
            )
            .await;
            if error {
                log::warn!("ql connection fatal error");
                cb.set_fatal_disconnect(true);
            }
        }
    });

    state.borrow_mut().ql_status_monitor = Some(task);
}

async fn check_firmware_update_available(state: StoredValue<AppState>) {
    log::info!("Checking for firmware update");

    let ui = state.borrow().ui();
    let callbacks = ui.global::<OnboardingCallbacks>();

    callbacks.set_firmware_check_state(FirmwareCheckState::Loading);

    let message = quantum_link::messages::CheckFirmwareUpdate;
    let task = state.borrow().ql_status.send_ql_archive(message);
    let update = match task.await {
        Ok(update) => update,
        Err(e) => {
            log::error!("failed to check for firmware update {e:?}");
            callbacks.set_firmware_check_state(FirmwareCheckState::Failed);
            return;
        }
    };

    let callbacks = ui.global::<OnboardingCallbacks>();
    match update {
        Some(update) => {
            callbacks.set_firmware_check_state(FirmwareCheckState::UpdateAvailable);
            callbacks.set_new_keyos_version(SharedString::from(update.version));
        }
        None => {
            log::error!("error occurred when processing firmware update.");
            callbacks.set_firmware_check_state(FirmwareCheckState::NoUpdateAvailable);
            callbacks.set_new_keyos_version(SharedString::default());
        }
    }
}

fn init_seed_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let seed_global = ui.global::<SeedGlobal>();

    seed_global.on_set_pin(move |pin, is_pin| {
        let mut state = state.borrow_mut();
        let pin_entry = if is_pin { PinEntryMode::Pin } else { PinEntryMode::Passphrase };
        let pin = RawPin(pin.to_string());
        state.pending_set_pin = Some(state::PendingPin { pin, pin_entry });
    });

    seed_global.on_is_pin_set(move || {
        let state = state.borrow();
        state.pending_set_pin.is_some()
    });

    seed_global.on_create_master_seed(move || {
        spawn_local(state::setup_seed::create_new_master_seed(state)).detach()
    });

    seed_global.on_restore_from_seed_qr(move || {
        spawn_local(async move {
            state::setup_seed::restore_from_seed_qr(state)
                .await
                .inspect_err(|e| log::error!("failed to restore from seedqr {e:?}"))
                .ok();
        })
        .detach()
    });

    seed_quiz::init_seed_callbacks!(ui);

    seed_global.on_restore_from_seed_words(move |words| {
        spawn_local(state::setup_seed::restore_from_seed_words(state, words)).detach();
    });

    seed_global.on_retry_restore_magic_backup(move || {
        let ql_status = state.borrow().ql_status.clone();
        spawn_worker(state::setup_seed::restore_magic_backup(ql_status)).detach();
    });

    seed_global.on_get_seed_words(move || {
        state
            .borrow()
            .try_get_mnemonic_words()
            .map(|words| {
                slint::ModelRc::new(slint::VecModel::from(
                    words.into_iter().map(slint::SharedString::from).collect::<Vec<_>>(),
                ))
            })
            .inspect_err(|e| log::error!("Failed to get seed words: {e}"))
            .unwrap_or_default()
    });

    seed_global.on_generate_mnemonic_order(move || {
        state
            .borrow()
            .mnemonic_order()
            .map(|order| {
                slint::ModelRc::new(slint::VecModel::from(
                    order.into_iter().map(|i| i as i32).collect::<Vec<_>>(),
                ))
            })
            .inspect_err(|e| log::error!("Failed to generate mnemonic order: {e}"))
            .unwrap_or_default()
    });

    seed_global.on_get_seed_word_challenge(move |mnemonic_index| {
        usize::try_from(mnemonic_index)
            .map_err(|_| anyhow::anyhow!("mnemonic_index {mnemonic_index} is negative"))
            .and_then(|idx| state.borrow().get_seed_word_challenge(idx).map(Into::into))
            .inspect_err(|e| log::error!("Failed to get seed word challenge: {e}"))
            .unwrap_or_default()
    });

    seed_global.on_get_standard_seed_qr(move || {
        get_standard_seed_qr(state)
            .inspect_err(|e| log::error!("Failed to generate standard seed QR: {e}"))
            .unwrap_or_default()
    });

    seed_global.on_get_compact_seed_qr(move || {
        get_compact_seed_qr(state)
            .inspect_err(|e| log::error!("Failed to generate compact seed QR: {e}"))
            .unwrap_or_default()
    });
}

fn init_backup(state: StoredValue<AppState>) {
    use keycard_scan::backup::BackupKind;
    use state::{keycard_backup::KeycardBackupFlow, keycard_restore::KeycardRestoreFlow};

    let ui = state.borrow().ui();
    let backup_global = ui.global::<KeycardBackupGlobal>();

    backup_global.on_start_manual_keycard_backup(move || {
        KeycardBackupFlow::start(state, BackupKind::Manual);
    });

    backup_global.on_start_magic_backup(move || {
        KeycardBackupFlow::start(state, BackupKind::Magic);
    });

    backup_global.on_error_clicked(move |confirm: bool| {
        KeycardBackupFlow::handle_error_click(state, confirm);
    });

    let restore_global = ui.global::<KeycardRestoreGlobal>();
    restore_global.on_start_keycard_restore(move || {
        KeycardRestoreFlow::start(state);
    });
}

fn init_connect_wallet(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let cb = ui.global::<OnboardingCallbacks>();

    cb.on_start_wallet_connect(move || {
        spawn_local(async move {
            match ql_utils::launch_bitcoin_app::<app_manager_permissions::AppManagerPermissions>().await {
                Ok(_) => {
                    log::info!("successfully launched bitcoin app");
                }
                e => {
                    log::error!("failed to launch bitcoin app {e:?}");
                }
            };
        })
        .detach();
    });

    cb.on_finish_onboarding(move || {
        spawn_local(async move {
            state.borrow().settings.set_onboarding_status(OnboardingStatus::Complete);
            let notify = state
                .borrow()
                .ql_status
                .send_ql_archive(NotifyOnboardingState { state: OnboardingState::Completed });
            timeout(notify, Duration::from_secs(5))
                .await
                .inspect_err(|_| log::warn!("failed to notify onboarding complete"))
                .ok();

            log::info!("Onboarding finished, switching to launcher");
            state.borrow_mut().finished = true;
            state
                .borrow()
                .gui
                .switch_to_launcher()
                .inspect_err(|e| log::warn!("failed to switch to launcher: {e:?}"))
                .ok();
        })
        .detach();
    });

    spawn_local(async move {
        let events = std::pin::pin!(subscribe_archive::<QuantumLinkPermissions, _>(
            quantum_link::messages::SubscribePublishedAccountUpdate
        ));
        // we only need the first published account
        let mainnet_account = events
            .filter_map(|event| RemoteUpdate::deserialize(&event.update).ok())
            .filter_map(|update| update.metadata)
            .find(|config| config.network == Network::Bitcoin)
            .await;
        if let Some(config) = mainnet_account {
            let ui = state.borrow().ui();
            let cb = ui.global::<OnboardingCallbacks>();
            cb.set_account_name(config.name.to_shared_string());
            cb.set_wallet_connected(true);
            notify_onboarding_state(state, OnboardingState::WalletConected);
        }
    })
    .detach();
}

impl From<seed_quiz::SeedWordChallenge> for SeedWordChallenge {
    fn from(c: seed_quiz::SeedWordChallenge) -> SeedWordChallenge {
        let options = ModelRc::from(c.options.map(SharedString::from));
        crate::SeedWordChallenge {
            correct_option_index: c.correct_option_index as i32,
            options,
            mnemonic_index: c.word_index as i32,
        }
    }
}

fn get_standard_seed_qr(state: StoredValue<AppState>) -> anyhow::Result<slint::Image> {
    let seed = state.borrow().try_get_seed()?;
    let data = seed.to_standard_seed_qr_data()?;
    let qr_image = slint_keyos_platform::qrcode::render(
        &data,
        slint::Color::from_rgb_u8(0, 0, 0),       // black
        slint::Color::from_rgb_u8(255, 255, 255), // white
    );
    Ok(qr_image)
}

fn get_compact_seed_qr(state: StoredValue<AppState>) -> anyhow::Result<slint::Image> {
    let seed = state.borrow().try_get_seed()?;
    let data = seed.to_compact_seed_qr_data()?;
    let qr_image = slint_keyos_platform::qrcode::render(
        &data,
        slint::Color::from_rgb_u8(0, 0, 0),       // black
        slint::Color::from_rgb_u8(255, 255, 255), // white
    );
    Ok(qr_image)
}

pub fn notify_onboarding_state(state: StoredValue<AppState>, onboarding_state: OnboardingState) {
    let msg = NotifyOnboardingState { state: onboarding_state };
    let mut state = state.borrow_mut();
    let task = spawn_worker(state.ql_status.send_ql_archive(msg));
    state.notify_onboarding_state = Some(task);
}

fn start_firmware_download(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let callbacks = ui.global::<OnboardingCallbacks>();
    callbacks.set_fw_update_state(FwUpdateState::Downloading);
    callbacks.set_fw_update_progress(0.0);
    callbacks.set_fw_update_eta(SharedString::default());
    callbacks.set_fw_update_error(FwUpdateError::DownloadFailed);

    let start_fw_update =
        state.borrow().ql_status.send_ql_archive_retry(StartFirmwareUpdate { chunk_offset: None }, |e| {
            log::warn!("failed to start fw update {e:?}, retrying...")
        });
    spawn_worker(async move {
        start_fw_update.await;
        log::info!("started fw update");
    })
    .detach();
}

pub fn notify_update_progress(state: StoredValue<AppState>, event: FirmwareInstallEvent) {
    let msg = NotifyFirmwareInstall { event };
    let mut state = state.borrow_mut();
    let task = spawn_worker(state.ql_status.send_ql_archive(msg));
    state.notify_onboarding_state = Some(task);
}

fn clear_slint_state(ui: &AppWindow, state: &AppState) {
    let ql_status = state.ql_status.current();
    let cb = ui.global::<OnboardingCallbacks>();

    cb.set_bt_connected(ql_status.bt_connected);
    cb.set_fatal_disconnect(Default::default());
    cb.set_show_restart_modal(Default::default());
    cb.set_security_state(Default::default());
    cb.set_current_keyos_version(Default::default());
    cb.set_new_keyos_version(Default::default());
    cb.set_firmware_check_state(Default::default());
    cb.set_fw_update_state(Default::default());
    cb.set_fw_update_error(Default::default());
    cb.set_fw_update_progress(Default::default());
    cb.set_fw_update_eta(Default::default());
    cb.set_wallet_connected(Default::default());
    cb.set_account_name(Default::default());
    cb.set_debug_actions(Default::default());

    let seed = ui.global::<SeedGlobal>();
    seed.set_master_seed_state(Default::default());
    seed.set_restore_backup_state(Default::default());
    seed.set_seed_qr_error(Default::default());
    seed.set_is_master_key_recovery(Default::default());
    seed.set_fingerprint_mismatch(Default::default());

    let keycard_backup = ui.global::<KeycardBackupGlobal>();
    keycard_backup.set_steps(Default::default());
    keycard_backup.set_saving_to_keycard(Default::default());
    keycard_backup.set_error(Default::default());

    let keycard_restore = ui.global::<KeycardRestoreGlobal>();
    keycard_restore.set_steps(Default::default());
    keycard_restore.set_reading_from_keycard(Default::default());
    keycard_restore.set_restore_kind(Default::default());
    keycard_restore.set_different_device_id(Default::default());

    let erase = ui.global::<EraseGlobal>();
    erase.set_progress(Default::default());
    erase.set_erasing_steps(Default::default());
}

fn erase_system_state() {
    let fs = FileSystem::default();
    match fs.remove(fs::SYSTEM_STATE_ROOT, fs::Location::System) {
        Ok(_) | Err(fs::Error::FileNotFound) => {}
        Err(e) => log::error!("Failed to erase system state dir: {e:?}"),
    }
}

fn update_temp_file() -> String { format!("{}/update.bin", fs::SYSTEM_STATE_ROOT) }
