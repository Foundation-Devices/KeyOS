// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Built-in command implementations

pub mod build;
pub mod cert;
pub mod clean;
pub mod completions;
pub mod develop;
pub mod doctor;
pub mod exit;
pub mod install;
pub mod logs;
pub mod new;
pub mod pack;
pub mod plugin;
pub mod preview;
pub mod search;
pub mod sideload;
pub mod sim;
pub mod theme;
pub mod themes;
pub mod uninstall;

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
