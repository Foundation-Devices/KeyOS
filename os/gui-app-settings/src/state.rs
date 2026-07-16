// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::{self, Context};
use bt::BtAddr;
use jiff::fmt::strtime;
use ngwallet::bdk_wallet::bitcoin::secp256k1::{All, Secp256k1};
use quantum_link::SendMessageError;
use slint_keyos_platform::{
    file_backed::JsonBacked,
    gui_server_api::msg::UpdateKioskPolicy,
    gui_server_api::navigation::filepicker::{AllowedExtensions, Location, SelectFileOptions},
    navigation::select_file,
    settings::global,
    slint::{self, ComponentHandle},
    PlatformConfig, TaskHandle,
};
use xous_api_ticktimer::TicktimerPrivileged;

use crate::{
    backup_permissions::BackupPermissions, fs_permissions::FileSystemPermissions,
    gui_permissions::GuiPermissions, timezones::TimeZoneModel, tr, AppWindow, BackupGlobal, BatteryGlobal,
    BluetoothApi, DateTimeGlobal, GuiApi, PowerManagerExtApi, QlStatus, QuantumLinkApi, Security,
    SettingsApi, UpdateApi,
};

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Status {
    pub last_envoy_comms: Option<SystemTime>,
}

pub struct AppState {
    pub gui: Arc<GuiApi>,
    pub settings: SettingsApi,
    pub fs: crate::FileSystem,
    pub security: Security,
    pub power_manager: PowerManagerExtApi,
    pub ticktimer: TicktimerPrivileged,
    pub bt: BluetoothApi,
    pub ql_status: QlStatus,

    pub secp: Secp256k1<All>,
    pub last_backup: Option<SystemTime>,

    pub ui: slint::Weak<AppWindow>,

    pub ble_address: BtAddr,
    pub quantum: QuantumLinkApi,

    pub timezone: Rc<TimeZoneModel>,
    pub update: UpdateApi,
    pub backup_api: backup::BackupApi<BackupPermissions>,

    pub notify_update_event: Option<TaskHandle<Result<(), SendMessageError>>>,

    pub backup_verification: Option<crate::keycard_verify::KeycardVerifyFlow>,
    pub keycard_backup: Option<crate::keycard_backup::KeycardBackupFlow>,
    pub persisted_status: JsonBacked<Status, FileSystemPermissions>,

    pub platform_config: Rc<PlatformConfig>,
}

impl AppState {
    pub fn new(gui: Arc<GuiApi>, ui: slint::Weak<AppWindow>, platform_config: Rc<PlatformConfig>) -> Self {
        Self {
            gui,
            settings: SettingsApi::default(),
            fs: crate::FileSystem::default(),
            security: Security::default(),
            power_manager: Default::default(),
            ticktimer: TicktimerPrivileged::default(),
            bt: BluetoothApi::default(),
            ql_status: QlStatus::new(slint_keyos_platform::worker().clone()),
            secp: Secp256k1::new(),
            last_backup: None,

            ui,

            ble_address: [0; 6].into(),
            quantum: QuantumLinkApi::default(),
            timezone: Rc::new(TimeZoneModel::new()),
            update: UpdateApi::default(),
            backup_api: backup::BackupApi::default(),
            notify_update_event: None,
            backup_verification: None,
            keycard_backup: None,
            persisted_status: JsonBacked::new("status.json", fs::Location::AppData).0,

            platform_config,
        }
    }

    pub fn ui(&self) -> AppWindow { self.ui.unwrap() }

    pub fn update_system_time<F>(&self, mapper: F)
    where
        F: FnOnce(jiff::Zoned) -> Option<jiff::Zoned>,
    {
        let tz = self.settings.get_time_zone();
        if let Some(new_zoned) = mapper(tz.now()) {
            self.ticktimer.set_system_time(new_zoned.timestamp().as_nanosecond() as u64);
            self.update_slint_time(new_zoned);
        }
    }

    pub fn refresh_time(&self) {
        let tz = self.settings.get_time_zone();
        self.update_slint_time(tz.now());
    }

    fn update_slint_time(&self, zoned: jiff::Zoned) {
        let ui = self.ui();
        let globals = ui.global::<DateTimeGlobal>();

        globals.set_day(zoned.day() as i32);
        globals.set_month(zoned.month() as i32);
        globals.set_year(zoned.year() as i32);
        globals.set_hour(zoned.hour() as i32);
        globals.set_minute(zoned.minute() as i32);
        globals.set_second(zoned.second() as i32);

        let display_time = if globals.get_time_24() {
            strtime::format("%H:%M", &zoned).unwrap()
        } else {
            strtime::format("%I:%M %p", &zoned).unwrap()
        };
        globals.set_display_time(display_time.into());
    }

    pub fn update_slint_timezone(&self, tz: global::TimeZone) {
        let ui = self.ui();
        let slint_tz = crate::TimeZone::new(tz, jiff::Timestamp::now());
        ui.global::<DateTimeGlobal>().set_selected_timezone(slint_tz);
    }

    pub fn refresh_battery_stats(&self) {
        let ui = self.ui();
        let globals = ui.global::<BatteryGlobal>();

        let Ok(status) = self.power_manager.status() else {
            return;
        };
        globals.set_battery_level(status.battery_percent as i32);

        let Some(extended_status) = self.power_manager.extended_status() else {
            return;
        };

        globals.set_remaining_capacity_mah(extended_status.remaining_capacity_mah as i32);
        globals.set_current_capacity_mah(extended_status.capacity_mah as i32);
        globals.set_voltage_mv(extended_status.voltage_mv as i32);
        globals.set_current_ma(extended_status.current as i32);

        if let Some(fault) = extended_status.last_reported_fault {
            globals.set_last_fault(format!("{:?}", fault).into());
            globals.set_has_fault(true);
        } else {
            globals.set_has_fault(false);
            globals.set_last_fault("".into());
        }
    }

    pub fn refresh_backup_stats(&self) {
        let ui = self.ui();
        let global = ui.global::<BackupGlobal>();

        let backup_duration = match self.last_backup {
            Some(timestamp) => {
                let duration = std::time::SystemTime::now().duration_since(timestamp).unwrap_or_else(|e| {
                    log::warn!("formatting a backup time in the future: {:?}", e);
                    Duration::ZERO
                });

                tr::format_duration(duration)
            }
            None => String::new(),
        };

        global.set_last_backup_duration(backup_duration.into());
    }

    pub fn save_log_files(&self) -> anyhow::Result<()> {
        let options = SelectFileOptions::default()
            .with_hidden_allowed(false)
            .with_dirs_allowed(true)
            .with_dir_selection_mode(true)
            .with_multiple_selection_mode(false)
            .with_allowed_extensions(AllowedExtensions::specific(&["txt"]));

        let (path, location) = select_file::<GuiPermissions>(options)
            .context("Failed to select directory")?
            .and_then(|selected| selected.files().get(0).cloned())
            .ok_or(anyhow::anyhow!("No directory selected"))?;

        let location = match location {
            Location::Internal => fs::Location::User,
            Location::External => fs::Location::Usb,
            Location::Airlock => fs::Location::Airlock,
        };

        for i in 0..9 {
            let source_file_path = format!(".log/log.{i}.log");
            let metadata = match self.fs.metadata(&source_file_path, fs::Location::User) {
                Ok(m) => m,
                Err(e) => {
                    log::info!("failed to fetch log metadata for log {}, skipping: {:?}", i, e);
                    continue;
                }
            };

            let (date, time) = (metadata.modified.date, metadata.modified.time);
            let date_time = format!(
                "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
                date.year, date.month, date.day, time.hour, time.min, time.sec
            );

            let file_path = format!("{}/{}_log.txt", path, date_time);

            if let Ok(_) = self.fs.open_file(
                &file_path,
                location,
                fs::OpenFlags { read: false, write: true, create: false },
            ) {
                log::info!("{} already exists, skipping", file_path);
                continue;
            }

            let mut source_file = match self.fs.open_file(
                &source_file_path,
                fs::Location::User,
                fs::OpenFlags { read: true, write: false, create: false },
            ) {
                Ok(sf) => sf,
                Err(e) => {
                    log::info!("failed to open log {}, skipping: {:?}", i, e);
                    continue;
                }
            };

            let mut dest_file = match self.fs.open_file(
                &file_path,
                location,
                fs::OpenFlags { read: false, write: true, create: true },
            ) {
                Ok(df) => df,
                Err(e) => {
                    log::info!("failed to create destination file {}, skipping: {:?}", file_path, e);
                    continue;
                }
            };

            source_file.copy_to(&mut dest_file).context("Failed to copy log file")?;
        }

        Ok(())
    }

    // cancel background tasks when the user navigates away.
    pub fn cancel_tasks(&mut self) {
        use crate::{RouteOption, RouteState};
        let ui = self.ui();
        let active_route = ui.global::<RouteState>().get_active();
        if active_route != RouteOption::VerifyBackup && self.backup_verification.take().is_some() {
            log::info!("Cancelled backup verification flow {active_route:?}");
        }
        if (active_route != RouteOption::CreateMagicBackup && active_route != RouteOption::CreateManualBackup)
            && self.keycard_backup.take().is_some()
        {
            log::info!("Cancelled keycard backup flow {active_route:?}");
        }

        // Ensure swipe back is available and auto-lock is restored when an update page is left
        if active_route != RouteOption::UpdateProgress {
            self.set_update_kiosk_enabled(true);
        }
    }

    pub fn set_update_kiosk_enabled(&self, enabled: bool) {
        self.gui.update_kiosk_policy(UpdateKioskPolicy::all(enabled)).ok();
        self.platform_config.enable_swipe_back.set(enabled);
    }
}
