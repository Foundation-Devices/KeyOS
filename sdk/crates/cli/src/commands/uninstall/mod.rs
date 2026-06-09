// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Uninstall a Foundation plugin

use anyhow::{Context, Result};
use foundation_plugins::install::{InstallError, PluginInstaller};

/// Execute the uninstall command
pub fn execute(plugin: &str) -> Result<()> {
    println!("Uninstalling plugin '{}'...", plugin);
    println!();

    let installer = PluginInstaller::new();

    match installer.uninstall(plugin) {
        Ok(()) => {
            println!("  \x1b[32m✓\x1b[0m {}", format!("Plugin '{}' uninstalled successfully!", plugin));
            Ok(())
        }
        Err(e) => {
            println!("  \x1b[31m✗\x1b[0m {}", format_error(&e, plugin));
            Err(e).context("Failed to uninstall plugin")
        }
    }
}

fn format_error(e: &InstallError, name: &str) -> String {
    match e {
        InstallError::NotInstalled(_) => format!("Plugin '{}' is not installed", name),
        InstallError::Io(e) => format!("IO error: {}", e.to_string()),
        _ => e.to_string(),
    }
}
