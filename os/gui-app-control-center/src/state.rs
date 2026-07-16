// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(feature = "recovery-os"))]
use std::time::{Duration, SystemTime};

#[cfg(not(feature = "recovery-os"))]
use i18n::replace_placeholders;
#[cfg(not(feature = "recovery-os"))]
use jiff::fmt::strtime;
#[cfg(not(feature = "recovery-os"))]
use slint_keyos_platform::settings::global;
use slint_keyos_platform::slint::ComponentHandle;

use crate::AppWindow;
#[cfg(not(feature = "recovery-os"))]
use crate::{tr, TrId};

// 2025.01.01 01:00:00 UTC
#[cfg(not(feature = "recovery-os"))]
const MINIMUM_TIME: jiff::Timestamp = jiff::Timestamp::constant(1735693200, 0);

pub struct AppState {
    #[cfg(not(feature = "recovery-os"))]
    pub timezone: global::TimeZone,
    #[cfg(not(feature = "recovery-os"))]
    pub use_standard_time_format: global::UseStandardTimeFormat,

    pub is_charging: bool,
    pub is_usb_attached: bool,
    pub battery_percent: u8,

    #[cfg(not(feature = "recovery-os"))]
    pub last_backup_at: Option<SystemTime>,

    #[cfg(not(feature = "recovery-os"))]
    pub settings: crate::SettingsApi,
    #[cfg(all(keyos, not(feature = "recovery-os")))]
    pub nfc: crate::NfcApi,
    #[cfg(all(keyos, not(feature = "recovery-os")))]
    pub camera: crate::CameraApi,
    #[cfg(keyos)]
    pub usb_host: crate::UsbHost,
    #[cfg(all(keyos, not(feature = "recovery-os")))]
    pub usb_device: crate::UsbDeviceEmulation,

    pub ui: AppWindow,
}

impl AppState {
    pub fn new(ui: AppWindow) -> Self {
        Self {
            #[cfg(not(feature = "recovery-os"))]
            timezone: global::TimeZone::default(),
            #[cfg(not(feature = "recovery-os"))]
            use_standard_time_format: global::UseStandardTimeFormat(true),
            is_charging: false,
            is_usb_attached: false,
            battery_percent: 100,
            #[cfg(not(feature = "recovery-os"))]
            last_backup_at: None,
            #[cfg(not(feature = "recovery-os"))]
            settings: Default::default(),
            #[cfg(all(keyos, not(feature = "recovery-os")))]
            nfc: Default::default(),
            #[cfg(all(keyos, not(feature = "recovery-os")))]
            camera: Default::default(),
            #[cfg(keyos)]
            usb_host: Default::default(),
            #[cfg(all(keyos, not(feature = "recovery-os")))]
            usb_device: Default::default(),
            ui,
        }
    }

    #[cfg(not(feature = "recovery-os"))]
    pub fn update_time(&self) {
        let ui = &self.ui;
        let timestamp = jiff::Timestamp::now();

        if timestamp < MINIMUM_TIME {
            ui.set_control_center_time("".into());
            ui.set_control_center_day_of_week("".into());
            ui.set_control_center_date("".into());
            return;
        }

        let tz = self.timezone.timezone();
        let zoned = timestamp.to_zoned(tz);

        let standard_time = self.use_standard_time_format.0;
        if standard_time {
            ui.set_control_center_time(strtime::format("%H:%M", &zoned).unwrap().into());
        } else {
            ui.set_control_center_time(strtime::format("%I:%M %p", &zoned).unwrap().into());
        }
        ui.set_control_center_day_of_week(strtime::format("%A", &zoned).unwrap().into());
        ui.set_control_center_date(strtime::format("%B %e, %Y", &zoned).unwrap().into());
    }

    #[cfg(not(feature = "recovery-os"))]
    pub fn update_system_msg(&self) {
        let ui = &self.ui;

        let backup_time = match self.last_backup_at {
            Some(timestamp) => {
                let duration = std::time::SystemTime::now().duration_since(timestamp).unwrap_or_else(|e| {
                    log::warn!("formatting a backup time in the future: {:?}", e);
                    Duration::ZERO
                });

                let duration_str = tr::format_duration(duration);
                replace_placeholders(&tr::lookup_id(TrId::CommonBackupBackedUpXAgo), &[duration_str])
            }
            None => tr::lookup_id(TrId::CommonBackupNeverBackedUp).to_string(),
        };

        ui.set_control_center_system_msg(backup_time.into());
    }

    pub fn slint_state(&self) -> crate::State<'_> { ComponentHandle::global::<crate::State>(&self.ui) }
}
