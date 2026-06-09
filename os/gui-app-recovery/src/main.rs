// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicBool, Ordering};

use slint_keyos_platform::{
    app,
    gui_server_api::navigation::filepicker::{
        AllowedExtensions, AllowedLocations, Location, SelectFileOptions,
    },
    navigation,
};

use crate::gui_permissions::GuiPermissions;

power_manager::use_api!();
power_manager::use_ext_api!();
#[cfg(keyos)]
recovery_worker::use_api!();
security::use_api!();
#[cfg(keyos)]
usb::use_host_api!();

/// The minimum battery percentage required for proceeding with recovery.
const MIN_BATTERY_PERCENT_FOR_RECOVERY: u8 = 50;

#[cfg(keyos)]
mod securam;
#[cfg(keyos)]
mod worker;

app!("Firmware Recovery", kind = Launcher);

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).expect("init log");
    log::set_max_level(log::LevelFilter::Info);

    log::trace!("Running the recovery app");

    #[cfg(keyos)]
    ui.global::<Callbacks>().on_reset_usb_drive(move || {
        UsbHost::default().set_enabled(false).ok();
        UsbHost::default().set_enabled(true).ok();
    });

    #[cfg(keyos)]
    ui.global::<Callbacks>().on_recovery_clicked({
        let ui = ui.clone_strong();
        move || {
            {
                ui.global::<RecoveryGlobal>().set_curr_recovery_step(RecoveryStep::Extracting);
                ui.global::<RecoveryGlobal>().set_recovery_progress(0.0);

                let worker_api = RecoveryWorkerApi::default();
                if let Err(e) = worker_api.start_recovery() {
                    log::error!("Error extracting the archive: {e:?}");
                    // TODO: surface the error to the user
                }
            }
        }
    });

    ui.global::<Callbacks>().on_has_sufficient_battery_charge(move || {
        let power_manager_api = PowerManagerExtApi::default();
        power_manager_api
            .status()
            .map(|status| status.battery_percent >= MIN_BATTERY_PERCENT_FOR_RECOVERY)
            .unwrap_or_default()
    });

    ui.global::<Callbacks>().on_reboot_clicked(move || {
        log::info!("Reboot clicked");
        PowerManagerApi::default().reboot();
    });

    ui.global::<Callbacks>().on_shut_down_clicked({
        let gui = cx.gui.clone();
        move || {
            log::info!("Shut down clicked");
            gui.shutdown().expect("shutdown GUI");
        }
    });

    #[cfg(keyos)]
    ui.global::<Callbacks>().on_listen_to_fs_events({
        let ui = ui.clone_strong();

        move || {
            UsbHost::default().set_enabled(true).ok();

            let ui = ui.clone_strong();
            slint_keyos_platform::spawn_local(async move {
                let mut sub = slint_keyos_platform::subscribe_scalar::<
                    fs_permissions::FileSystemPermissions,
                    _,
                >(fs::messages::SubscribeFilesystemEvent(fs::Location::Usb));
                while let Some(event) = sub.next().await {
                    fs_event_handler(event, ui.clone_strong());
                }
            })
            .detach();
        }
    });

    #[cfg(keyos)]
    ui.global::<Callbacks>().on_reset_all_settings_and_data({
        let ui = ui.clone_strong();
        move || {
            let ui = ui.clone_strong();
            ui.global::<EraseGlobal>().set_is_erasing_settings(false);
            ui.global::<EraseGlobal>().set_is_error(false);
            ui.global::<EraseGlobal>().set_erase_progress(0.0);
            slint_keyos_platform::spawn_local(async move {
                log::info!("Erasing everything");

                erase_system_state();

                let lockout_result = futures_lite::future::or(
                    slint_keyos_platform::async_archive::<security_permissions::SecurityPermissions, _>(
                        security::messages::Lockout {
                            lockout_options: security::LockoutOptions::erase_all(),
                            reboot: false,
                        },
                    ),
                    async {
                        // Since we don't get actual progress from Security,
                        // fake progress for around 3 seconds
                        let mut progress = 0.0;
                        loop {
                            ui.global::<EraseGlobal>().set_erase_progress(progress);
                            slint_keyos_platform::sleep(core::time::Duration::from_millis(100)).await;
                            progress = (progress + 0.03).min(0.95);
                        }
                    },
                )
                .await;

                ui.global::<EraseGlobal>().set_is_error(lockout_result.is_err());
                ui.global::<EraseGlobal>().set_erase_progress(1.0);
            })
            .detach();
        }
    });

    #[cfg(keyos)]
    {
        // Choose the mode from OS arguments
        let os_arguments = crate::securam::os_arguments();

        if let Ok(securam_manager::OsArguments::SystemInfoMode {
            bootloader_version,
            bootloader_build_date,
        }) = os_arguments
        {
            // Wait for the splash screen animation to finish
            std::thread::sleep(std::time::Duration::from_secs(1));

            let bootloader_version_str = String::from_utf8_lossy(&bootloader_version).to_string();
            ui.global::<InfoGlobal>().set_bootloader_version(bootloader_version_str.into());
            let bootloader_build_date =
                chrono::DateTime::from_timestamp(bootloader_build_date as i64, 0).unwrap();
            let bootloader_build_date_str = bootloader_build_date.format("%b %d %Y").to_string();
            ui.global::<InfoGlobal>().set_bootloader_build_date(bootloader_build_date_str.into());

            log::info!("Navigating to system info screen");

            // Navigate the UI to the system info screen
            let navigate = ui.global::<Navigate>();
            navigate.invoke_info(NavigateOptions { replace: true, ..Default::default() });

            worker::system_info::subscribe_bootloader(ui.clone_strong());
            worker::system_info::subscribe_keyos(ui.clone_strong());
            worker::system_info::subscribe_recovery(ui.clone_strong());
        } else {
            worker::recovery::init(ui.clone_strong());
        }
    }

    ui.run().map_err(|e| anyhow::anyhow!("Platform error: {:?}", e)).expect("run ui");
}

static HAS_POPUP_OPENED: AtomicBool = AtomicBool::new(false);

#[cfg(keyos)]
fn fs_event_handler(event: fs::FileSystemEvent, ui: AppWindow) {
    if event.location != fs::Location::Usb {
        return;
    }
    let is_connected = event.event_type == fs::FileSystemEventType::Mounted;
    if is_connected {
        log::info!("Mass storage connected");
    } else {
        log::info!("Mass storage disconnected");
        HAS_POPUP_OPENED.store(false, Ordering::Relaxed);

        let recovery_global = ui.global::<RecoveryGlobal>();
        if !recovery_global.get_is_tar_copied() {
            recovery_global.set_new_keyos_selected(false);
            recovery_global.set_new_keyos_valid(false);
            recovery_global.set_new_keyos_validation_in_progress(false);
            recovery_global.set_new_keyos_validation_error(false);
            recovery_global.set_new_keyos_validation_error_str("".into());
            recovery_global.set_is_usb_drive_connected(false);
            recovery_global.set_is_tar_copied(false);
        }
    }

    let is_usb_enabled = UsbHost::default().is_enabled().unwrap_or(false);
    let is_usb_available = is_usb_enabled && is_connected;

    // Don't schedule a new task if the popup is already opened
    if HAS_POPUP_OPENED.load(Ordering::Relaxed) {
        return;
    }

    slint_keyos_platform::spawn_local(async move {
        ui.global::<RecoveryGlobal>().set_is_usb_drive_connected(is_usb_available);

        if is_usb_available {
            select_file(&ui);
        }
    })
    .detach();
}

fn select_file(ui: &AppWindow) {
    let popup_result = file_selection_popup();
    HAS_POPUP_OPENED.store(false, Ordering::Relaxed);

    if let Some((selected_file, selected_location)) = popup_result {
        log::info!("Selected file path: {selected_file}");
        if selected_location != Location::External {
            log::error!("Location was not 'External'. This should never happen");
            return;
        }

        let display_path = selected_file.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/");
        ui.global::<RecoveryGlobal>().set_new_keyos_file_path(display_path.into());
        ui.global::<RecoveryGlobal>().set_is_usb_drive_connected(true);
        ui.global::<RecoveryGlobal>().set_new_keyos_valid(false);
        ui.global::<RecoveryGlobal>().set_new_keyos_validation_step(RecoveryValidationStep::Reading);
        ui.global::<RecoveryGlobal>().set_recovery_progress(0f32);
        ui.global::<RecoveryGlobal>().set_new_keyos_validation_in_progress(true);
        ui.global::<RecoveryGlobal>().set_new_keyos_selected(true);

        #[cfg(keyos)]
        {
            let recovery_worker_api = RecoveryWorkerApi::default();
            recovery_worker_api.read_recovery_archive(&selected_file, fs::Location::Usb);
        }
    } else {
        log::info!("Popup dismissed");

        // TODO: maybe a different screen if the user dismissed the popup without disconnecting the
        // USB drive
        ui.global::<RecoveryGlobal>().set_is_usb_drive_connected(false);
    }
}

fn file_selection_popup() -> Option<(String, Location)> {
    HAS_POPUP_OPENED.store(true, Ordering::Relaxed);

    log::debug!("Opening file selection popup");
    let options = SelectFileOptions::default()
        .with_hidden_allowed(false)
        .with_search_allowed(false)
        .with_start_location(Location::External)
        .with_allowed_locations(AllowedLocations::specific([Location::External]))
        .with_allowed_extensions(AllowedExtensions::specific(["bin"]));

    let res = navigation::select_file::<GuiPermissions>(options).expect("select file navigation")?;
    log::debug!("File selection popup result: {:?}", res);

    res.files().first().map(|(file, location)| (file.to_owned(), *location))
}

fn erase_system_state() {
    let fs = FileSystem::default();
    match fs.remove(fs::SYSTEM_STATE_ROOT, fs::Location::System) {
        Ok(_) | Err(fs::Error::FileNotFound) => {}
        Err(e) => log::error!("Failed to erase system state dir: {e:?}"),
    }
}
