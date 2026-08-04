// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod macros;

use std::time::Duration;

use macros::*;

macros::settings_registry!(
    system: [
        SystemTheme: scalar,
        ScreenBrightness: scalar,
        Locale: scalar,
        DeviceName: archive,
        ShowSecurityWords: scalar,
        AutoLock: scalar,
        EnvoyTimeSync: scalar,
        UseStandardTimeFormat: scalar,
        TimeZone: archive,
        DebugTouch: scalar,
        AirlockMode: scalar,
        TouchOffset: scalar,
        MagicBackupEnabled: scalar,
        NfcEnabled: scalar,
        BluetoothEnabled: scalar,
        CameraEnabled: scalar,
        UsbEnabled: scalar,
    ],
    encrypted: [
        OnboardingStatus: archive,
        DeveloperMode: scalar,
    ],
);

/// crate private trait for loading default values
/// guaranteed to only be called from settings server
pub(crate) trait LoadDefault {
    fn load_default() -> Self;
}

impl LoadDefault for settings::global::SystemTheme {
    fn load_default() -> Self { load_prime_color() }
}

impl LoadDefault for settings::global::ScreenBrightness {
    fn load_default() -> Self {
        #[cfg(keyos)]
        {
            Self(70)
        }

        #[cfg(not(keyos))]
        {
            Self(100)
        }
    }
}

impl LoadDefault for settings::global::OnboardingStatus {
    fn load_default() -> Self { Self::NotComplete }
}

impl LoadDefault for settings::global::Locale {
    fn load_default() -> Self { Self::EnglishUS }
}

impl LoadDefault for settings::global::DeviceName {
    fn load_default() -> Self { Self(String::from(settings::global::DeviceName::DEFAULT)) }
}

impl LoadDefault for settings::global::ShowSecurityWords {
    fn load_default() -> Self { Self(false) }
}

impl LoadDefault for settings::global::AutoLock {
    fn load_default() -> Self { Self(Duration::from_secs(300)) }
}

impl LoadDefault for settings::global::EnvoyTimeSync {
    fn load_default() -> Self { Self(true) }
}

impl LoadDefault for settings::global::UseStandardTimeFormat {
    fn load_default() -> Self { Self(false) }
}

impl LoadDefault for settings::global::TimeZone {
    fn load_default() -> Self {
        let (name, data) = jiff_tzdb::get("America/New_York").unwrap();
        settings::global::TimeZone { name: name.into(), data: data.to_vec() }
    }
}

impl LoadDefault for settings::global::DebugTouch {
    fn load_default() -> Self { Self(false) }
}

impl LoadDefault for settings::global::AirlockMode {
    fn load_default() -> Self { Self::ReadWrite }
}

impl LoadDefault for settings::global::TouchOffset {
    #[cfg(keyos)]
    fn load_default() -> Self { Self(-30) }

    // See SFT-5550
    #[cfg(not(keyos))]
    fn load_default() -> Self { Self(0) } // simulator is touch perfect
}

impl LoadDefault for settings::global::MagicBackupEnabled {
    fn load_default() -> Self { Self(true) }
}

impl LoadDefault for settings::global::NfcEnabled {
    fn load_default() -> Self { Self(true) }
}

impl LoadDefault for settings::global::BluetoothEnabled {
    fn load_default() -> Self { Self(true) }
}

impl LoadDefault for settings::global::CameraEnabled {
    fn load_default() -> Self { Self(true) }
}

impl LoadDefault for settings::global::UsbEnabled {
    fn load_default() -> Self { Self(true) }
}

impl LoadDefault for settings::global::DeveloperMode {
    fn load_default() -> Self { Self(!cfg!(feature = "production")) }
}

pub(crate) fn load_board_revision() -> settings::global::BoardRevision {
    use std::sync::OnceLock;
    static REVISION: OnceLock<settings::global::BoardRevision> = OnceLock::new();

    *REVISION.get_or_init(|| {
        #[cfg(keyos)]
        {
            let Ok(sfc_mem) = xous::map_memory(
                xous::MemoryAddress::new(utralib::HW_SFC_BASE),
                None,
                0x1000,
                xous::MemoryFlags::W | xous::MemoryFlags::DEV,
            ) else {
                return settings::global::BoardRevision::D6;
            };

            let sfc = atsama5d27::sfc::Sfc::with_alt_base_addr(sfc_mem.as_ptr() as u32);
            let revision = match fuse::get_board_revision(&sfc) {
                fuse::BoardRevision::RevD1 => settings::global::BoardRevision::D1,
                fuse::BoardRevision::RevD6 => settings::global::BoardRevision::D6,
            };
            xous::unmap_memory(sfc_mem).expect("unmap SFC memory");
            revision
        }

        #[cfg(not(keyos))]
        {
            settings::global::BoardRevision::D6
        }
    })
}

pub(crate) fn load_prime_color() -> settings::global::SystemTheme {
    use std::sync::OnceLock;
    static COLOR: OnceLock<settings::global::SystemTheme> = OnceLock::new();

    *COLOR.get_or_init(|| {
        #[cfg(keyos)]
        {
            let Ok(sfc_mem) = xous::map_memory(
                xous::MemoryAddress::new(utralib::HW_SFC_BASE),
                None,
                0x1000,
                xous::MemoryFlags::W | xous::MemoryFlags::DEV,
            ) else {
                return settings::global::SystemTheme::Dark;
            };

            let sfc = atsama5d27::sfc::Sfc::with_alt_base_addr(sfc_mem.as_ptr() as u32);
            let res = match fuse::get_colorway(&sfc).unwrap_or(fuse::Colorway::Dark) {
                fuse::Colorway::Light => settings::global::SystemTheme::Light,
                fuse::Colorway::Dark => settings::global::SystemTheme::Dark,
            };
            xous::unmap_memory(sfc_mem).expect("unmap SFC memory");
            res
        }

        #[cfg(not(keyos))]
        {
            settings::global::SystemTheme::Dark
        }
    })
}
