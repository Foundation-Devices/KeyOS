// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use bt::messages::TestEcho;
use haptics::HapticPattern;
use keycard::messages::IdentifyKeycard;
use quantum_link::messages::StartRestoreMagicBackup;
use rgb_led::RgbColor;
use slint_keyos_platform::{
    app, async_archive, fs,
    gui_server_api::{
        navigation::filepicker::{Location, SelectFileOptions},
        ControlCenterColor,
    },
    navigation::select_file,
    sleep, spawn_local, StoredValue,
};

bt::use_api!();
haptics::use_api!();
rgb_led::use_api!();
backup::use_api!();
keycard::use_api!();
nfc::use_api!();
quantum_link::use_api!();

app!("Developer Playground");

use gui_permissions::GuiPermissions;
use quantum_link_permissions::QuantumLinkPermissions;

use crate::bt_permissions::BluetoothPermissions;

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("Starting Developer Playground");

    let haptics_api = HapticsApi::default();
    let rgb_api = RgbApi::default();
    let backup_api = BackupApi::default();

    ui.global::<Callbacks>().on_play_haptic_pattern(move |pattern| {
        log::info!("Playing haptic pattern: {}", pattern);

        let haptic_pattern = match pattern.as_str() {
            "StrongClick100" => HapticPattern::StrongClick100,
            "StrongClick60" => HapticPattern::StrongClick60,
            "StrongClick30" => HapticPattern::StrongClick30,
            "SharpClick100" => HapticPattern::SharpClick100,
            "SharpClick60" => HapticPattern::SharpClick60,
            "SharpClick30" => HapticPattern::SharpClick30,
            "SoftBump100" => HapticPattern::SoftBump100,
            "SoftBump60" => HapticPattern::SoftBump60,
            "SoftBump30" => HapticPattern::SoftBump30,
            "DoubleClick100" => HapticPattern::DoubleClick100,
            "DoubleClick60" => HapticPattern::DoubleClick60,
            "TripleClick100" => HapticPattern::TripleClick100,
            "SoftFuzz60" => HapticPattern::SoftFuzz60,
            "StrongBuzz100" => HapticPattern::StrongBuzz100,
            "Alert750ms" => HapticPattern::Alert750ms,
            "Alert1000ms" => HapticPattern::Alert1000ms,
            _ => {
                log::warn!("Unknown haptic pattern: {}", pattern);
                return;
            }
        };

        haptics_api.vibrate(haptic_pattern);
    });

    ui.global::<Callbacks>().on_set_led(move |led, r, g, b| {
        log::info!("Setting LED {} to RGB({}, {}, {})", led, r, g, b);

        let color = RgbColor::new(r as u8, g as u8, b as u8);
        rgb_api.set_to(led as u32, color);
    });

    ui.global::<Callbacks>().on_set_control_center_color({
        let gui = cx.gui.clone();
        move |r, g, b| {
            let color = ControlCenterColor::new(r as u8, g as u8, b as u8);
            if let Err(error) = gui.set_control_center_color(Some(color)) {
                log::error!("Failed to set Control Center color: {error:?}");
            }
        }
    });

    ui.global::<Callbacks>().on_reset_control_center_color({
        let gui = cx.gui.clone();
        move || {
            if let Err(error) = gui.set_control_center_color(None) {
                log::error!("Failed to reset Control Center color: {error:?}");
            }
        }
    });

    let weak_ui = ui.as_weak();
    ui.global::<Callbacks>().on_start_keycard_formatter(move || {
        log::info!("Starting Keycard Formatter");
        let weak_ui = weak_ui.clone();
        spawn_local(async move {
            // Enable NFC and initialize UI state
            let _ = NfcApi::default().set_enabled(true);
            let ui = weak_ui.unwrap();
            ui.global::<Callbacks>().set_formatter_formatting(false);
            ui.global::<Callbacks>().set_formatter_success(false);

            let mut last_uid: Option<Vec<u8>> = None;

            loop {
                match async_archive::<keycard_permissions::KeycardPermissions, _>(IdentifyKeycard).await {
                    Ok((id, _)) => {
                        let uid = id.0.clone();
                        // Skip re-formatting if the card is the same as the last one we formatted
                        if last_uid.as_ref().map_or(false, |prev| prev == &uid) {
                            continue;
                        }

                        // Start formatting animation
                        let ui = weak_ui.unwrap();
                        ui.global::<Callbacks>().set_formatter_success(false);
                        ui.global::<Callbacks>().set_formatter_formatting(true);

                        log::info!("Formatting keycard: {:02x?}", uid);
                        match async_archive::<keycard_permissions::KeycardPermissions, _>(
                            keycard::messages::FormatKeycard(id),
                        )
                        .await
                        {
                            Ok(()) => {
                                HapticsApi::default().vibrate(HapticPattern::StrongClick100);
                                log::info!("Keycard formatted successfully");

                                last_uid = Some(uid);

                                let ui = weak_ui.unwrap();
                                ui.global::<Callbacks>().set_formatter_formatting(false);
                                ui.global::<Callbacks>().set_formatter_success(true);

                                // Keep the checkmark visible for 1 second
                                sleep(Duration::from_secs(1)).await;

                                let ui = weak_ui.unwrap();
                                ui.global::<Callbacks>().set_formatter_success(false);
                            }
                            Err(e) => {
                                // Stop animating on error
                                let ui = weak_ui.unwrap();
                                ui.global::<Callbacks>().set_formatter_formatting(false);
                                ui.global::<Callbacks>().set_formatter_success(false);
                                log::error!("FormatKeycard failed: {:#}", e);
                            }
                        }
                    }
                    Err(e) => {
                        // No card (or error) -> allow the same card to be formatted again once re-presented
                        last_uid = None;
                        let ui = weak_ui.unwrap();
                        ui.global::<Callbacks>().set_formatter_formatting(false);
                        // Don't toggle success here, so we can preserve the success state until it times out
                        log::error!("IdentifyKeycard failed: {:#}", e);
                    }
                }
            }
        })
        .detach();
    });

    let ui_backup = ui.as_weak();
    ui.global::<Callbacks>().on_create_backup(move || {
        log::info!("Creating backup");
        let ui = ui_backup.unwrap();
        ui.global::<Callbacks>().set_backup_state(BackupState::BackingUp);

        match backup_api.create_backup() {
            Ok(_) => {
                log::info!("Backup created");
                ui.global::<Callbacks>().set_backup_state(BackupState::Success);
            }
            Err(e) => {
                log::error!("Failed to create backup: {:?}", e);
                ui.global::<Callbacks>().set_backup_state(BackupState::Failure);
            }
        }
    });

    let selected_file = Rc::new(RefCell::new(None::<(String, fs::Location)>));

    ui.global::<Callbacks>().on_select_backup_file({
        let ui = ui.as_weak();
        let selected_file = selected_file.clone();
        move || {
            log::info!("Selecting backup file");
            match select_file::<GuiPermissions>(SelectFileOptions::default()) {
                Ok(Some(result)) => {
                    if result.files.is_empty() {
                        log::info!("No file selected");
                        return;
                    }

                    let (path, location) = &result.files[0];
                    log::info!("Selected backup file: {} at location {:?}", path, location);

                    let fs_location = match location {
                        Location::Internal => fs::Location::User,
                        Location::Airlock => fs::Location::Airlock,
                        Location::External => fs::Location::Usb,
                    };

                    *selected_file.borrow_mut() = Some((path.clone(), fs_location));

                    let ui = ui.unwrap();
                    ui.global::<Callbacks>().set_restore_path(path.clone().into());
                }
                Ok(None) => log::info!("No file selected"),
                Err(e) => {
                    log::error!("Failed to select file: {:?}", e);
                }
            }
        }
    });

    ui.global::<Callbacks>().on_restore_backup({
        let backup_api = BackupApi::default();
        let ui = ui.as_weak();
        let selected_file = selected_file.clone();
        move || {
            let ui = ui.unwrap();
            ui.global::<Callbacks>().set_backup_state(BackupState::Restoring);

            let file_info = selected_file.borrow().clone();

            let result = (|| -> Result<(), String> {
                let (path, location) = file_info.ok_or("No file selected")?;
                log::info!("Restoring backup from: {} at location {:?}", path, location);

                let system_path = if location != fs::Location::System {
                    log::info!("Copying backup file to system partition");
                    let filename = path.split('/').last().unwrap_or("backup.tar.enc");
                    let system_path = format!("/backup_restore_{}", filename);

                    let fs = FileSystem::default();

                    let mut src = fs
                        .open_file(&path, location, fs::OpenFlags { read: true, write: false, create: false })
                        .map_err(|e| format!("Failed to open source file: {:?}", e))?;

                    let mut dst = fs
                        .open_file(
                            &system_path,
                            fs::Location::System,
                            fs::OpenFlags { read: true, write: true, create: true },
                        )
                        .map_err(|e| format!("Failed to create destination file: {:?}", e))?;

                    std::io::copy(&mut src, &mut dst).map_err(|e| format!("Failed to copy file: {:?}", e))?;

                    log::info!("Copied backup file to {}", system_path);
                    system_path
                } else {
                    path
                };

                backup_api
                    .restore_backup(system_path, fs::Location::System)
                    .map_err(|e| format!("Failed to restore backup: {:?}", e))?;

                Ok(())
            })();

            match result {
                Ok(()) => {
                    log::info!("Backup restored successfully");
                    ui.global::<Callbacks>().set_restore_path("".into());
                    *selected_file.borrow_mut() = None;
                    ui.global::<Callbacks>().set_backup_state(BackupState::Success);
                }
                Err(e) => {
                    log::error!("Failed to restore backup: {}", e);
                    ui.global::<Callbacks>().set_backup_state(BackupState::Failure);
                }
            }
        }
    });

    ui.global::<Callbacks>().on_restore_from_envoy(move || {
        log::info!("Restoring backup from Envoy");

        spawn_local(async move {
            async_archive::<QuantumLinkPermissions, _>(StartRestoreMagicBackup)
                .await
                .inspect_err(|e| log::info!("failed to send StartRestoreMagicBackup {e:?}"))
                .ok();
        })
        .detach();
    });

    setup_bt(&ui);

    ui.run().expect("UI running");
}

fn setup_bt(ui: &AppWindow) {
    let spi_task = StoredValue::new(None);
    ui.global::<Callbacks>().on_bt_spi_test_start({
        let ui = ui.clone_strong();
        move |size| {
            let ui = ui.clone_strong();
            ui.global::<Callbacks>().set_bt_spi_test_status(format!("Starting").into());
            *spi_task.borrow_mut() = Some(spawn_local(async move {
                let mut character = 0x55;
                let mut measurement_start = Instant::now();
                let mut packets = 0;
                loop {
                    match async_archive::<BluetoothPermissions, _>(TestEcho { size: size as _, character })
                        .await
                    {
                        Ok(_) => {
                            packets += 1;
                            character += 1;
                            let now = Instant::now();
                            if now.duration_since(measurement_start) >= Duration::from_secs(1) {
                                let seconds = now.duration_since(measurement_start).as_secs_f32();
                                let pps = packets as f32 / seconds;
                                let kbps = packets as f32 * size as f32 / 1000.0 / seconds;
                                ui.global::<Callbacks>()
                                    .set_bt_spi_test_status(format!("{pps:.1} pps\n{kbps:.1} kBps").into());
                                measurement_start = now;
                                packets = 0;
                            }
                        }
                        Err(e) => {
                            ui.global::<Callbacks>().set_bt_spi_test_status(format!("Error: {e:?}").into());
                            return;
                        }
                    }
                }
            }));
        }
    });
    ui.global::<Callbacks>().on_bt_spi_test_stop({
        let ui = ui.clone_strong();
        move || {
            *spi_task.borrow_mut() = None;
            ui.global::<Callbacks>().set_bt_spi_test_status(format!("Stopped").into());
        }
    });
}
