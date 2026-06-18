// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod screengrab;
pub mod settings;
pub mod theme;
slint::include_modules!();

pub const SIMULATOR_DIR: &str = "../../simulator-files";
pub const SETTINGS_FILE: &str = "../../simulator-files/settings.json";
pub const DEP_SETTINGS_FILE: &str = "../../simulator-files/deprecated_settings.json";
pub const SCREENSHOTS_DIR: &str = "screenshots";
pub const SCREENSHOTS_DIR_ENV: &str = "FOUNDATION_SIMULATOR_SCREENSHOTS_DIR";
const GIF_DELAY_MS: u32 = 80;
