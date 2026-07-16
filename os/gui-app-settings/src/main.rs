// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io::{Read, Seek},
    rc::Rc,
    time::{Duration, SystemTime},
};

use anyhow::{self, Context};
use bip39::Mnemonic;
use ngwallet::{bdk_wallet::bitcoin::Network, bip39::MasterKey};
use quantum_link::{
    foundation_api::firmware::{FirmwareInstallEvent, InstallErrorStage},
    messages::{NotifyFirmwareInstall, SendPrimeMagicBackupEnabled, StartFirmwareUpdate},
    PairingEvent,
};
use security::{messages::Lockout, OsVersionInfo, PinEntryMode};
use slint_keyos_platform::{
    app, async_archive,
    futures_lite::StreamExt as _,
    gui_server_api::{
        msg::UpdateKioskPolicy,
        navigation::{
            filepicker::{AllowedExtensions, Location, SelectFileOptions},
            lockscreen::{VerifyPinOptions, VerifyPinResult},
        },
    },
    navigation::select_file,
    navigation::verify_pin,
    settings::{self, global::SystemTheme},
    slint::{Image, ModelRc, SharedString, Timer, TimerMode, VecModel},
    spawn_local, spawn_worker, subscribe_archive, subscribe_scalar, timeout, StoredValue, TaskHandle,
};
use update::messages::ProgressUpdate;

use crate::{
    backup_permissions::BackupPermissions, gui_permissions::GuiPermissions,
    quantum_link_permissions::QuantumLinkPermissions, security_permissions::SecurityPermissions,
    settings_permissions::SettingsPermissions, state::AppState,
};

mod keycard_backup;
mod keycard_verify;
mod state;
mod timezones;

app_manager::use_api!();
backup::use_api!();
bt::use_api!();
haptics::use_api!();
keycard::use_api!();
power_manager::use_ext_api!();
quantum_link::use_api!();
security::use_api!();
update::use_api!();

const PERIODIC_UPDATE_INTERVAL: Duration = Duration::from_millis(1000);

app!("Settings", kind = Settings);

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let state = StoredValue::new(AppState::new(cx.gui.clone(), ui.as_weak(), cx.config.clone()));

    ql_utils::on_ble_address(state.borrow().bt.clone(), move |addr| {
        log::info!("got bt address: {addr:?}");
        state.borrow_mut().ble_address = addr;
    });

    // cancel outstanding tasks, if any
    cx.router.borrow_mut().register_on_navigation_end(move |_| {
        spawn_local(async move {
            state.borrow_mut().cancel_tasks();
        })
        .detach();
    });

    setup_settings_global(state);
    setup_datetime_globals(state);
    setup_about_global(state);
    setup_pin_global(state);
    setup_log_global(state);
    setup_backup_global(state);
    setup_keycard_backup_global(state);
    setup_ql_global(state);
    setup_update_global(state);
    setup_callbacks(state);
    setup_save_settings_global(state);
    resume_update_if_needed(state);

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, PERIODIC_UPDATE_INTERVAL, move || {
        let state = state.borrow();
        state.refresh_time();
        state.refresh_battery_stats();
        state.refresh_backup_stats();
    });

    ui.run().expect("UI running");
}

fn setup_settings_global(state: StoredValue<AppState>) {
    spawn_local({
        let state = state.clone();
        async move {
            let mut sub = subscribe_scalar::<settings_permissions::SettingsPermissions, _>(
                settings::messages::SubscribeScreenBrightness,
            );
            while let Some(brightness) = sub.next().await {
                let state = state.borrow();
                let ui = state.ui();
                ui.global::<SettingGlobal>().set_screen_brightness(brightness.0 as f32);
            }
        }
    })
    .detach();

    spawn_local({
        let state = state.clone();
        async move {
            let mut sub = subscribe_archive::<settings_permissions::SettingsPermissions, _>(
                settings::messages::SubscribeDeviceName,
            );
            while let Some(device_name) = sub.next().await {
                let state = state.borrow();
                let ui = state.ui();
                ui.global::<SettingGlobal>().set_device_name(device_name.0.into());
            }
        }
    })
    .detach();

    let ui = state.borrow().ui();
    let globals = ui.global::<SettingGlobal>();

    globals.on_set_dark_mode(move |dark_mode| {
        let theme = if dark_mode { SystemTheme::Dark } else { SystemTheme::Light };
        log::info!("Setting theme: {:?}", theme);
        state.borrow().settings.set_system_theme(theme);
    });

    globals.set_device_name(state.borrow().settings.get_device_name().0.into());
    globals.on_set_device_name(move |device_name| {
        let state = state.borrow();
        state.settings.set_device_name(device_name.as_str());
        let ui = state.ui();
        ui.global::<SettingGlobal>().set_device_name(device_name);
    });

    globals.on_set_screen_brightness(move |brightness| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<SettingGlobal>().set_screen_brightness(brightness);
        let brightness = brightness as u8;
        state.settings.set_screen_brightness(brightness);
    });

    globals.set_auto_lock(state.borrow().settings.get_auto_lock().0.as_secs() as i32);

    globals.on_set_auto_lock(move |auto_lock| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<SettingGlobal>().set_auto_lock(auto_lock);
        let auto_lock = Duration::from_secs(auto_lock as u64);
        state.settings.set_auto_lock(auto_lock);
    });
    globals.on_format_auto_lock(move |seconds| {
        // TODO: translation
        if seconds == -1 {
            return "Never".into();
        }
        let minutes = seconds / 60;

        if minutes > 59 {
            let hours = minutes / 60;
            let hours_str: SharedString = hours.to_string().into();
            if hours == 1 {
                return format!("{hours_str} {}", tr::lookup_id(TrId::CommonTimeHourFull)).into();
            }
            return format!("{hours_str} {}", tr::lookup_id(TrId::CommonTimeHoursFull)).into();
        }
        let minutes_str: SharedString = minutes.to_string().into();
        if minutes == 1 {
            return format!("{minutes_str} {}", tr::lookup_id(TrId::CommonTimeMinuteFull)).into();
        }
        return format!("{minutes_str} {}", tr::lookup_id(TrId::CommonTimeMinutesFull)).into();
    });

    globals.set_show_security_words(state.borrow().settings.get_show_security_words().0);
    globals.on_set_show_security_words(move |show_security_words| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<SettingGlobal>().set_show_security_words(show_security_words);
        state.settings.set_show_security_words(show_security_words);
    });

    globals.on_factory_reset(move || {
        spawn_local(async move {
            let ui = state.borrow().ui();
            let nav = ui.global::<Navigate>();

            nav.invoke_erase_device(
                EraseDeviceParams { status: EraseStatus::Progress },
                NavigateOptions { animate: Animate::None, replace: true },
            );

            // Best-effort goodbye to Envoy before wiping. Awaits BLE flush so
            // the bye is on the wire before erase_system_state() / Lockout reboots.
            if let Err(e) =
                async_archive::<QuantumLinkPermissions, _>(quantum_link::messages::UnpairFromEnvoy).await
            {
                log::warn!("failed to notify Envoy of unpair before factory reset: {e:?}");
            }
            erase_system_state();

            match async_archive::<SecurityPermissions, _>(Lockout {
                lockout_options: security::LockoutOptions::erase_all(),
                reboot: true,
            })
            .await
            {
                Ok(_) => {
                    // We should never get to this branch because lockout will reboot
                    log::info!("successfully reset device");
                }
                Err(_) => {
                    log::error!("failed to factory reset");
                }
            }
        })
        .detach();
    });

    let version = state.borrow().security.os_version_info().map_or_else(
        |_| "unknown".to_string(),
        |opt| {
            opt.map(|info| String::from_utf8_lossy(&info.keyos_version).to_string())
                .unwrap_or_else(|| "N/A".to_string())
        },
    );
    globals.set_current_keyos_version(SharedString::from(version));
}

fn setup_datetime_globals(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let dt_globals = ui.global::<DateTimeGlobal>();

    dt_globals.set_time_24(state.borrow().settings.get_use_standard_time_format().0);
    dt_globals.on_set_time_24(move |time_24| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<DateTimeGlobal>().set_time_24(time_24);
        state.settings.set_use_standard_time_format(time_24);
    });

    dt_globals.set_envoy_time_sync(state.borrow().settings.get_envoy_time_sync().0);
    dt_globals.on_set_envoy_time_sync(move |envoy_time_sync| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<DateTimeGlobal>().set_envoy_time_sync(envoy_time_sync);
        state.settings.set_envoy_time_sync(envoy_time_sync);
        if envoy_time_sync {
            ql_utils::sync_system_timezone(state.settings.clone(), state.ql_status.clone(), |e| {
                log::warn!("failed to retrieve tz from envoy {e:?}")
            })
            .detach();
        }
    });

    dt_globals.set_timezone_search_list(ModelRc::new(state.borrow().timezone.clone()));

    dt_globals.on_timezone_search_text_edited(move |search_text| {
        state.borrow().timezone.set_search(&search_text);
    });

    dt_globals.on_date_changed(move |y: i32, m: i32, d: i32| {
        state
            .borrow()
            .update_system_time(|current| current.with().year(y as _).month(m as _).day(d as _).build().ok());
    });

    dt_globals.on_time_changed(move |hh: i32, mm: i32, ss: i32| {
        state.borrow().update_system_time(|current| {
            current.with().hour(hh as _).minute(mm as _).second(ss as _).build().ok()
        });
    });
    {
        let state = state.borrow();
        let tz = state.settings.get_time_zone();
        state.update_slint_timezone(tz);
    }

    dt_globals.on_timezone_selected(move |timezone| {
        let state = state.borrow();
        let timezone = String::from(timezone.id);
        let tz = state.settings.lookup_timezone(timezone, 0);
        state.settings.set_time_zone(tz.clone());
        state.update_slint_timezone(tz);
    });
}

fn setup_pin_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let pin_global = ui.global::<PinGlobal>();

    pin_global.on_verify_pin(move |title, want_words| {
        let res = verify_pin::<GuiPermissions>(VerifyPinOptions {
            title: Some(title.into()),
            want_security_words: want_words,
        });

        if want_words {
            let ui = state.borrow().ui();
            let pin_global = ui.global::<PinGlobal>();

            match &res {
                Ok(VerifyPinResult { success: true, security_words: Some([w0, w1]), .. }) => {
                    pin_global.set_last_security_words(ModelRc::from([
                        SharedString::from(w0),
                        SharedString::from(w1),
                    ]));
                }
                _ => pin_global.set_last_security_words(Default::default()),
            }
        }

        res.map(|r| r.success).unwrap_or_else(|e| {
            log::error!("verify_pin failed: {e}");
            false
        })
    });

    pin_global.on_change_pin(move |new_pin, is_pin| {
        let state = state.borrow();
        let mode = if is_pin { PinEntryMode::Pin } else { PinEntryMode::Passphrase };
        state.ui().global::<PinGlobal>().set_is_pin_entry(is_pin);
        state.security.change_pin(new_pin.as_str().to_owned(), None, mode).is_ok()
    });

    let pin_entry_mode = state.borrow().security.get_pin_entry_mode();
    pin_global.set_is_pin_entry(pin_entry_mode == PinEntryMode::Pin);
}

fn setup_log_global(state: StoredValue<AppState>) {
    const MAX_LINE_LEN: usize = 59;

    let ui = state.borrow().ui();
    let log_global = ui.global::<LogGlobal>();
    let lines = Rc::new(VecModel::<SharedString>::default());
    let fs = FileSystem::default();
    let mut log_file_offset = 0;
    log_global.set_log_lines(ModelRc::from(lines.clone()));
    log_global.on_update_log_lines(move || {
        let mut file = match fs.open_file(
            ".log/log.0.log",
            fs::Location::User,
            fs::OpenFlags { read: true, write: false, create: false },
        ) {
            Ok(f) => f,
            Err(e) => {
                log::error!("Could not open log file: {e:?}");
                return;
            }
        };
        let size = file.metadata().unwrap().size;
        // Check if log file was rotated
        if size < log_file_offset {
            log_file_offset = 0;
        } else if log_file_offset == size {
            return;
        }
        file.seek(std::io::SeekFrom::Start(log_file_offset)).ok();
        let mut contents = vec![0u8; (size - log_file_offset) as usize];
        if let Err(e) = file.read_exact(&mut contents) {
            log::error!("Could not read log file: {e:?}");
            return;
        }
        // Manual layouting: first split into the actual found newlines, then
        // split into MAX_LINE_LEN chunks.
        // Add extra newlines after each actual log line for better readability.
        for line in contents.split(|&p| p == b'\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(mut line) = str::from_utf8(line) else {
                log::error!("Log line was not utf-8");
                continue;
            };
            while !line.is_empty() {
                let (chunk, rest) = split_at_char(line, MAX_LINE_LEN);
                lines.push(chunk.into());
                line = rest;
            }
            lines.push("".into());
        }
        log_file_offset = size;
    });
}

fn split_at_char(s: &str, index: usize) -> (&str, &str) {
    let byte_index = s.char_indices().nth(index).map(|(i, _)| i).unwrap_or(s.len());

    s.split_at(byte_index)
}

fn setup_about_global(state: StoredValue<AppState>) {
    let mut state = state.borrow_mut();
    let ui = state.ui();
    let globals = ui.global::<AboutGlobal>();

    let Ok(version_info) = state.security.os_version_info() else {
        return;
    };

    match version_info {
        None => {
            globals.set_bootloader_version("N/A".into());
            globals.set_keyos_version("N/A".into());
        }

        Some(OsVersionInfo { bootloader_version, keyos_version }) => {
            let bootloader_version = String::from_utf8_lossy(&bootloader_version).to_string();
            let keyos_version = String::from_utf8_lossy(&keyos_version).to_string();
            globals.set_bootloader_version(bootloader_version.into());
            globals.set_keyos_version(keyos_version.into());
        }
    }
    let Some(version_info) = state.bt.get_version_info() else {
        return;
    };

    globals.set_ble_bootloader_version(version_info.bootloader_version.into());
    if let Some(firmware_version) = version_info.firmware_version {
        globals.set_ble_firmware_version(firmware_version.into());
    } else {
        globals.set_ble_firmware_version("N/A".into());
    }

    if let Ok(device_id) = &state.security.device_id() {
        globals.set_serial_number(device_id.to_string().into());
    } else {
        log::error!("Failed to get serial number");
        globals.set_serial_number("N/A".into());
    }

    if let Ok(key) = get_master_key(&state) {
        let fingerprint = key.fingerprint.to_string().to_uppercase();
        globals.set_master_fingerprint(fingerprint.clone().into());
        let reversed_fingerprint = fingerprint
            .as_bytes()
            .chunks(2)
            .rev()
            .map(|b| std::str::from_utf8(b).unwrap())
            .collect::<String>();
        globals.set_reversed_fingerprint(reversed_fingerprint.into());
    } else {
        log::error!("Failed to get fingerprint");
        globals.set_master_fingerprint("N/A".into());
        globals.set_reversed_fingerprint("N/A".into());
    }
}

fn get_master_key(app_state: &AppState) -> anyhow::Result<MasterKey> {
    let entropy = match app_state.security.seed() {
        Ok(Some(e)) => e,
        Ok(None) => anyhow::bail!("No seed found"),
        Err(e) => anyhow::bail!("Could not get seed: {:?}", e),
    };

    MasterKey::from_entropy(&app_state.secp, Network::Bitcoin, entropy.bytes(), "", None)
        .map_err(|e| anyhow::anyhow!("Could not derive seed: {}", e))
}

fn setup_callbacks(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let callbacks = ui.global::<Callbacks>();
    callbacks.on_save_log_files(move || match state.borrow().save_log_files() {
        Ok(_) => true,
        Err(e) => {
            log::error!("Failed to save log file: {}", e);
            false
        }
    });
    callbacks.on_close_settings(move || {
        if let Err(e) = state.borrow().gui.switch_to_launcher() {
            log::error!("Failed to switch to launcher: {}", e);
        }
    });

    callbacks.on_get_seed_words(move || {
        let app_state = state.borrow();
        let key = match get_master_key(&app_state) {
            Ok(k) => k,
            Err(e) => {
                log::error!("Failed to get master key: {}", e);
                return ModelRc::new(VecModel::from(vec![]));
            }
        };

        let words = key.mnemonic.split(' ').map(SharedString::from).collect::<Vec<SharedString>>();

        ModelRc::new(VecModel::from(words))
    });

    callbacks.on_get_standard_seed_qr(move || {
        let app_state = state.borrow();
        let key = match get_master_key(&app_state) {
            Ok(k) => k,
            Err(e) => {
                log::error!("Failed to get master key: {}", e);
                return Image::default();
            }
        };

        let mnemonic = match Mnemonic::parse(&key.mnemonic) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("Could not parse mnemonic: {:?}", e);
                return Image::default();
            }
        };

        let indices: String = mnemonic.word_indices().map(|idx| format!("{:04}", idx)).collect();
        slint_keyos_platform::qrcode::render(
            indices.as_bytes(),
            slint::Color::from_rgb_u8(0, 0, 0),
            slint::Color::from_rgb_u8(255, 255, 255),
        )
    });

    callbacks.on_get_compact_seed_qr(move || {
        let app_state = state.borrow();
        let key = match get_master_key(&app_state) {
            Ok(k) => k,
            Err(e) => {
                log::error!("Failed to get master key: {}", e);
                return Image::default();
            }
        };

        let mnemonic = match Mnemonic::parse(&key.mnemonic) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("Could not parse mnemonic: {:?}", e);
                return Image::default();
            }
        };

        slint_keyos_platform::qrcode::render(
            &mnemonic.to_entropy(),
            slint::Color::from_rgb_u8(0, 0, 0),
            slint::Color::from_rgb_u8(255, 255, 255),
        )
    });
}

fn setup_backup_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let backup_global = ui.global::<BackupGlobal>();

    backup_global.set_magic_backup_enabled(state.borrow().settings.get_magic_backup_enabled().0);

    backup_global.on_set_magic_backup_enabled(move |enabled| {
        let state = state.borrow();
        let ui = state.ui();
        ui.global::<BackupGlobal>().set_magic_backup_enabled(enabled);
        state.settings.set_magic_backup_enabled(enabled);
    });

    backup_global.on_create_backup(move || {
        spawn_local(async move {
            let ui = state.borrow().ui();
            let global = ui.global::<BackupGlobal>();
            global.set_status(BackupStatus::Creating);
            match timeout(
                async_archive::<BackupPermissions, _>(backup::messages::CreateBackup),
                Duration::from_secs(15),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    log::warn!("create backup failed {e:?}");
                    global.set_status(BackupStatus::Error);
                }
                Err(_) => {
                    log::warn!("create backup timed out");
                    global.set_status(BackupStatus::Error);
                }
            }
        })
        .detach();
    });

    spawn_local(async move {
        let mut status_updates =
            subscribe_scalar::<backup_permissions::BackupPermissions, _>(backup::messages::StatusSubscribe);
        while let Some(status) = status_updates.next().await {
            log::info!("Backup status update: {status:?}");
            let mut state = state.borrow_mut();
            state.last_backup = status.last_backup_at;

            let ui = state.ui();
            let global = ui.global::<BackupGlobal>();
            global.set_status(if status.publish_failed { BackupStatus::Error } else { BackupStatus::Idle });
        }
    })
    .detach();

    spawn_local(async move {
        let mut sub =
            subscribe_scalar::<SettingsPermissions, _>(settings::messages::SubscribeMagicBackupEnabled);
        let mut task: Option<TaskHandle<()>> = None;
        while let Some(magic_backup_enabled) = sub.next().await {
            let enabled = magic_backup_enabled.0;
            let ui = state.borrow().ui();
            let global = ui.global::<BackupGlobal>();
            global.set_magic_backup_enabled(enabled);
            let publish = state
                .borrow()
                .ql_status
                .send_ql_archive_retry(SendPrimeMagicBackupEnabled { enabled }, |e| {
                    log::warn!("failed to publish magic backup enabled {e:?}")
                });
            let _ = task.insert(spawn_worker(async move {
                publish.await;
                log::info!("published magic backup enabled");
            }));
        }
    })
    .detach();

    ui.global::<VerifyKeycardBackupGlobal>().on_start(move || {
        keycard_verify::KeycardVerifyFlow::start(state);
    });
}

fn setup_keycard_backup_global(state: StoredValue<AppState>) {
    use keycard_scan::backup::BackupKind;

    let ui = state.borrow().ui();
    let backup_global = ui.global::<KeycardBackupGlobal>();

    backup_global.on_start_manual_keycard_backup(move || {
        keycard_backup::KeycardBackupFlow::start(state, BackupKind::Manual);
    });

    backup_global.on_start_magic_backup(move || {
        keycard_backup::KeycardBackupFlow::start(state, BackupKind::Magic);
    });

    backup_global.on_error_clicked(move |confirm: bool| {
        keycard_backup::KeycardBackupFlow::handle_error_click(state, confirm);
    });
}

fn setup_ql_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let ql_global = ui.global::<QlGlobal>();

    spawn_local({
        let mut ql = state.borrow().ql_status.clone();
        async move {
            while let Some(status) = ql.next().await {
                log::info!("ql_status {status:?}");
                let ui = state.borrow().ui();
                let global = ui.global::<QlGlobal>();

                global.set_bt_connected(status.bt_connected);
                global.set_ql_paired(status.ql_paired);
                global.set_ql_live(status.live);
            }
        }
    })
    .detach();

    ql_global.on_qr_data(move || {
        let state = state.borrow();
        ql_utils::static_qr(&state.settings, &state.ble_address, true).into()
    });

    ql_global.on_animated_qr_data(move || {
        let state = state.borrow();
        ql_utils::animated_qr(&state.quantum)
    });

    ql_global.on_disconnect(move || {
        spawn_local(async move {
            if let Err(e) =
                async_archive::<QuantumLinkPermissions, _>(quantum_link::messages::UnpairFromEnvoy).await
            {
                log::warn!("failed to notify Envoy of unpair: {e:?}");
            }
        })
        .detach();
    });

    spawn_local(async move {
        let mut pairing_events =
            subscribe_archive::<QuantumLinkPermissions, _>(quantum_link::messages::SubscribePairingEvent);
        while let Some(pairing_event) = pairing_events.next().await {
            log::info!("pairing event: {pairing_event:?}");
            let ui = state.borrow().ui();
            let global = ui.global::<QlGlobal>();

            match pairing_event {
                PairingEvent::PairingComplete { device_name, new } => {
                    global.set_paired_device_name(device_name.into());
                    if new {
                        let s = state.borrow();
                        if s.settings.get_envoy_time_sync().0 {
                            ql_utils::sync_system_timezone(s.settings.clone(), s.ql_status.clone(), |e| {
                                log::warn!("failed to retrieve tz from envoy {e:?}")
                            })
                            .detach();
                        }
                        drop(s);
                        ql_utils::launch_bitcoin_app::<app_manager_permissions::AppManagerPermissions>()
                            .await
                            .inspect_err(|e| log::warn!("failed to start bitcoin app {e:?}"))
                            .ok();
                    }
                }
                PairingEvent::Disconnected => {}
                PairingEvent::RequestReceived => {}
                PairingEvent::PairingFailed => {}
            }
        }
    })
    .detach();

    spawn_local(async move {
        let ql_status = state.borrow().ql_status.clone();
        loop {
            ql_status.ready().await;
            ql_status.wait_until(|s| !s.bt_connected || !s.ql_paired || !s.live).await;
            let mut state = state.borrow_mut();
            let mut status_guard = state.persisted_status.guard();
            status_guard.last_envoy_comms = Some(SystemTime::now());
        }
    })
    .detach();
}

fn setup_update_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let update_global = ui.global::<UpdateGlobal>();

    update_global.on_check_fw_update(move || {
        spawn_local(check_firmware_update_available(state)).detach();
    });

    update_global.on_download_firmware_update(move || {
        start_firmware_download(state);
    });

    ql_utils::on_update_sufficient_battery::<power_manager_ext_permissions::PowerManagerExtPermissions, _>(
        move |sufficient_battery| {
            log::info!("update sufficient_battery={}", sufficient_battery);
            let ui = state.borrow().ui();
            ui.global::<UpdateGlobal>().set_update_sufficient_battery(sufficient_battery);
        },
    )
    .detach();

    spawn_local(async move {
        let mut update_events = subscribe_archive::<update_permissions::UpdatePermissions, _>(
            update::messages::SubscribeUpdateProgress,
        );

        let ql_status = state.borrow().ql_status.clone();
        let mut disconnect_monitor: Option<TaskHandle<()>> = None;

        while let Some(event) = update_events.next().await {
            // Keep auto-lock disabled until the user leaves the update page.
            let restore_update_exit_controls = || {
                let state = state.borrow();
                state
                    .gui
                    .update_kiosk_policy(
                        UpdateKioskPolicy::default()
                            .set_home_button(true)
                            .set_power_button(true)
                            .set_control_center(true),
                    )
                    .ok();
                state.platform_config.enable_swipe_back.set(true);
            };
            let ui = state.borrow().ui();
            let update_global = ui.global::<UpdateGlobal>();

            match event {
                ProgressUpdate::DownloadProgress(progress) => {
                    update_global.set_fw_update_state(FwUpdateState::Receiving);
                    update_global.set_fw_update_progress(progress.completion_percentage() as f32);

                    // Disable auto-lock and start monitoring for disconnection when download starts
                    if progress.is_start() {
                        state
                            .borrow()
                            .gui
                            .update_kiosk_policy(UpdateKioskPolicy::default().set_auto_lock(false))
                            .ok();
                        state.borrow().platform_config.enable_swipe_back.set(false);
                        let status = ql_status.clone().into_inner().into_stream();
                        let _ = disconnect_monitor.insert(spawn_local(async move {
                            std::pin::pin!(status).any(|status| !status.live).await;
                            log::error!("QuantumLink disconnected during update");
                            handle_update_error(
                                state,
                                "Connection lost".to_string(),
                                FwUpdateError::DownloadFailed,
                                InstallErrorStage::Download,
                            );
                        }));
                    }
                }
                ProgressUpdate::DownloadComplete => {
                    log::info!("update download complete");
                    disconnect_monitor = None;
                    notify_update_progress(state, FirmwareInstallEvent::Installing);
                    state.borrow().update.apply_downloaded_update();
                    update_global.set_fw_update_state(FwUpdateState::Installing);
                }
                ProgressUpdate::InstallProgress(progress) => {
                    update_global.set_fw_update_state(FwUpdateState::Installing);

                    let percent = progress.completion_percentage();
                    let secs_remaining = progress.estimate_time_remaining_secs();
                    let mins_remaining = secs_remaining.div_ceil(60).max(1);
                    let time_str = format!("{mins_remaining}m");

                    log::info!("update install progress {percent}% {time_str}");

                    update_global.set_fw_update_progress(percent as f32);
                    update_global.set_fw_update_eta(time_str.into());
                }
                ProgressUpdate::Rebooting => {
                    log::info!("update rebooting");
                    update_global.set_fw_update_state(FwUpdateState::Restarting);
                    notify_update_progress(state, FirmwareInstallEvent::Rebooting);
                }
                ProgressUpdate::Done => {
                    log::info!("update complete. rebooting...");
                    update_global.set_fw_update_state(FwUpdateState::Restarting);
                    notify_update_progress(state, FirmwareInstallEvent::Rebooting);
                    state.borrow().set_update_kiosk_enabled(true);
                }
                ProgressUpdate::InstallError(error) => {
                    disconnect_monitor = None;
                    log::error!("failed to apply update {error:?}");
                    restore_update_exit_controls();
                    handle_update_error(
                        state,
                        error.to_string(),
                        FwUpdateError::InstallFailed,
                        InstallErrorStage::Install,
                    );
                }
                ProgressUpdate::DownloadError(error) => {
                    disconnect_monitor = None;
                    log::error!("failed to download update {error:?}");
                    restore_update_exit_controls();
                    handle_update_error(
                        state,
                        error.to_string(),
                        FwUpdateError::DownloadFailed,
                        InstallErrorStage::Download,
                    );
                }
            }
        }
    })
    .detach();
}

fn resume_update_if_needed(state: StoredValue<AppState>) {
    let state = state.borrow();
    if !state.update.update_status().needs_continue {
        return;
    }

    let ui = state.ui();
    let update_global = ui.global::<UpdateGlobal>();
    if update_global.get_fw_update_state() == FwUpdateState::Installing {
        return;
    }

    log::info!("continuing interrupted update");
    state.set_update_kiosk_enabled(false);

    update_global.set_fw_update_state(FwUpdateState::Installing);
    update_global.set_fw_update_progress(0.0);
    update_global.set_fw_update_eta(SharedString::default());

    ui.global::<Navigate>().invoke_update_progress(NavigateOptions { animate: Animate::None, replace: true });
    state.update.continue_update();
}

fn setup_save_settings_global(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let global = ui.global::<SaveSettingsGlobal>();

    global.on_save_settings_file(move || {
        spawn_local(async move {
            if let Err(e) = save_settings_file(state).await {
                let ui = state.borrow().ui();
                let global = ui.global::<SaveSettingsGlobal>();
                log::error!("Failed to save settings file: {:?}", e);
                global.set_status(SaveSettingsStatus::Error);
            }
        })
        .detach();
    });
}

fn start_firmware_download(state: StoredValue<AppState>) {
    let ui = state.borrow().ui();
    let update_global = ui.global::<UpdateGlobal>();
    update_global.set_fw_update_state(FwUpdateState::Downloading);
    update_global.set_fw_update_progress(0.0);
    update_global.set_fw_update_eta(SharedString::default());
    update_global.set_fw_update_error(FwUpdateError::DownloadFailed);

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

fn handle_update_error(
    state: StoredValue<AppState>,
    error: String,
    fw_error: FwUpdateError,
    stage: InstallErrorStage,
) {
    let ui = state.borrow().ui();
    let update_global = ui.global::<UpdateGlobal>();
    update_global.set_fw_update_state(FwUpdateState::Failed);
    update_global.set_fw_update_error(fw_error);
    notify_update_progress(state, FirmwareInstallEvent::Error { error, stage });
}

async fn check_firmware_update_available(state: StoredValue<AppState>) {
    log::info!("Checking for firmware update");

    let ui = state.borrow().ui();
    let global = ui.global::<UpdateGlobal>();
    let ql_status = state.borrow().ql_status.clone();

    global.set_checking_fw_update(true);
    let result = timeout(
        ql_status.send_ql_archive(quantum_link::messages::CheckFirmwareUpdate),
        Duration::from_secs(10),
    )
    .await;
    global.set_checking_fw_update(false);

    let update = match result {
        Ok(Ok(update)) => update,
        Ok(Err(e)) => {
            log::error!("failed to check for firmware update {e:?}");
            global.set_new_keyos_version(SharedString::default());
            return;
        }
        Err(_) => {
            log::error!("timed out checking for firmware update");
            global.set_new_keyos_version(SharedString::default());
            return;
        }
    };

    let now = jiff::Timestamp::now();
    let tz = state.borrow().settings.get_time_zone();
    let zoned = now.to_zoned(tz.timezone());
    let last_checked =
        jiff::fmt::strtime::format("%Y-%m-%d %H:%M", &zoned).unwrap_or_else(|_| "Unknown".to_string());
    global.set_last_update_checked_on(last_checked.into());

    match update {
        Some(update) => {
            log::info!("firmware update available: {}", update.version);
            global.set_new_keyos_version(SharedString::from(&update.version));
        }
        None => {
            log::info!("no firmware update available");
            global.set_new_keyos_version(SharedString::default());
        }
    }
}

fn notify_update_progress(state: StoredValue<AppState>, event: FirmwareInstallEvent) {
    let msg = NotifyFirmwareInstall { event };
    let mut state = state.borrow_mut();
    let task = spawn_worker(state.ql_status.send_ql_archive(msg));
    state.notify_update_event = Some(task);
}

async fn save_settings_file(state: StoredValue<AppState>) -> anyhow::Result<()> {
    let state = state.borrow();
    let ui = state.ui();
    let global = ui.global::<SaveSettingsGlobal>();

    global.set_status(SaveSettingsStatus::Saving);

    let options = SelectFileOptions::default()
        .with_hidden_allowed(false)
        .with_dirs_allowed(true)
        .with_dir_selection_mode(true)
        .with_multiple_selection_mode(false)
        .with_allowed_extensions(AllowedExtensions::specific(&["tar"]));

    let (path, location) = select_file::<GuiPermissions>(options)
        .context("Failed to select a directory")?
        .and_then(|selected| selected.files().get(0).cloned())
        .ok_or(anyhow::anyhow!("No file selected"))?;

    let location = match location {
        Location::Internal => fs::Location::User,
        Location::External => fs::Location::Usb,
        Location::Airlock => fs::Location::Airlock,
    };

    let now = jiff::Timestamp::now();
    let tz = state.settings.get_time_zone();
    let zoned = now.to_zoned(tz.timezone());
    let timestamp =
        jiff::fmt::strtime::format("%Y-%m-%d_%H-%M-%S", &zoned).unwrap_or_else(|_| "unknown".to_string());
    let backup_path = format!("{}/settings-{}.tar", path, timestamp);

    state
        .backup_api
        .create_backup_file(backup_path.clone(), location)
        .context("Failed to create a backup")?;

    global.set_status(SaveSettingsStatus::Success);
    global.set_backup_path(backup_path.into());

    Ok(())
}

fn erase_system_state() {
    let fs = FileSystem::default();
    match fs.remove(fs::SYSTEM_STATE_ROOT, fs::Location::System) {
        Ok(_) | Err(fs::Error::FileNotFound) => {}
        Err(e) => log::error!("Failed to erase system state dir: {e:?}"),
    }
}
