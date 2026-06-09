// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod screengrab;
pub mod settings;
pub mod theme;
slint::include_modules!();

gui_server_api::use_api!();

pub const SIMULATOR_DIR: &str = "../../simulator-files";
pub const SETTINGS_FILE: &str = "../../simulator-files/settings.json";
pub const DEP_SETTINGS_FILE: &str = "../../simulator-files/deprecated_settings.json";
pub const SCREENSHOTS_DIR: &str = "screenshots";
pub const SCREENSHOTS_DIR_ENV: &str = "FOUNDATION_SIMULATOR_SCREENSHOTS_DIR";
const GIF_DELAY_MS: u32 = 80;

/// Quit the entire hosted simulator by mirroring the terminal Ctrl-C (SIGINT):
/// the hosted kernel spawns every service in one process group, so signalling
/// the group tears down the device-screen window, the kernel, and this control
/// panel together — not just the focused window.
pub fn quit_simulator() {
    #[cfg(unix)]
    // SAFETY: kill(2) with pid 0 sends SIGINT to our whole process group, the
    // same teardown the terminal delivers on Ctrl-C.
    unsafe {
        libc::kill(0, libc::SIGINT);
    }
    #[cfg(not(unix))]
    std::process::exit(0);
}
