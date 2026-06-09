// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicU16, Ordering};

use crate::Location;

bitflags::bitflags! {
    struct LocationFlags: u16 {
        const COMMON_ASSETS = 1 << 0;
        const APP_DATA = 1 << 1;
        const SYSTEM = 1 << 2;
        const ENCRYPTED_ROOT = 1 << 3;
        const BOOT = 1 << 4;
        const USB = 1 << 5;
        const USER = 1 << 6;
        const AIRLOCK = 1 << 7;
        const SYSTEM_APP_DATA = 1 << 8;
        const APP_RESOURCES = 1 << 9;
    }
}

#[derive(Debug, Default)]
pub struct AccessFlags {
    flags: AtomicU16,
}

impl Clone for AccessFlags {
    fn clone(&self) -> Self { Self { flags: AtomicU16::new(self.flags.load(Ordering::Relaxed)) } }
}

impl AccessFlags {
    pub fn contains(&self, location: Location) -> bool {
        let mask = flag_for(location).bits();
        (self.flags.load(Ordering::Relaxed) & mask) != 0
    }

    pub fn insert(&self, location: Location) {
        let mask = flag_for(location).bits();
        self.flags.fetch_or(mask, Ordering::Relaxed);
    }
}

#[inline]
fn flag_for(location: Location) -> LocationFlags {
    match location {
        Location::CommonAssets => LocationFlags::COMMON_ASSETS,
        Location::AppData => LocationFlags::APP_DATA,
        Location::System => LocationFlags::SYSTEM,
        Location::EncryptedRoot => LocationFlags::ENCRYPTED_ROOT,
        Location::Boot => LocationFlags::BOOT,
        Location::Usb => LocationFlags::USB,
        Location::User => LocationFlags::USER,
        Location::Airlock => LocationFlags::AIRLOCK,
        Location::SystemAppData => LocationFlags::SYSTEM_APP_DATA,
        Location::AppResources => LocationFlags::APP_RESOURCES,
    }
}
