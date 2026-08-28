// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Built-in command implementations

use foundation_core::AppConfig;

pub mod build;
pub mod cert;
pub mod clean;
pub mod completions;
pub mod develop;
pub mod docs;
pub mod doctor;
pub mod exit;
#[cfg(feature = "experimental-plugins")]
pub mod install;
pub mod logs;
pub mod new;
pub mod pack;
#[cfg(feature = "experimental-plugins")]
pub mod plugin;
pub mod preview;
#[cfg(feature = "experimental-plugins")]
pub mod search;
pub mod sideload;
pub mod sim;
pub mod skills;
pub mod theme;
pub mod themes;
#[cfg(feature = "experimental-plugins")]
pub mod uninstall;
pub mod update;

/// Render a byte count for a build artifact, e.g. `3.4 MiB`.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];

    for next_unit in UNITS.iter().skip(1) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next_unit;
    }

    if unit == "B" {
        format!("{bytes} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

pub(crate) fn warn_legacy_app_config_version(config: &AppConfig) {
    if config.version.is_some() {
        eprintln!(
            "Warning: `version` in app-config.toml is deprecated; remove it and set the app version only in Cargo.toml."
        );
    }
}
