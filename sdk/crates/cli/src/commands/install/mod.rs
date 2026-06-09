// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Install a Foundation plugin

use anyhow::{Context, Result};
use foundation_plugins::install::{InstallError, PluginInstaller};

/// Execute the install command
pub async fn execute(plugin: &str) -> Result<()> {
    println!("Installing plugin '{}'...", plugin);
    println!();

    let installer = PluginInstaller::new();

    // Show index path being used
    println!("  \x1b[2m{}\x1b[0m", "Reading plugin index...");

    match installer.install(plugin).await {
        Ok(installed) => {
            println!();
            println!(
                "  \x1b[32m✓\x1b[0m {}",
                format!("Plugin '{}' installed successfully!", &installed.name)
            );
            println!();
            println!("    {} {}", "Installed at:", installed.path.display());
            println!();
            println!("Run 'foundation {}' to use the plugin.", &installed.name);

            Ok(())
        }
        Err(e) => {
            println!();
            println!("  \x1b[31m✗\x1b[0m {}", format_error(&e));
            Err(e).context("Failed to install plugin")
        }
    }
}

fn format_error(e: &InstallError) -> String {
    match e {
        InstallError::NotFound(name) => format!("Plugin '{}' not found in registry", name),
        InstallError::ReleaseNotFound(name) => format!("No release found for plugin '{}'", name),
        InstallError::NoPlatformBinary(platform) => {
            format!("No binary available for your platform ({})", platform)
        }
        InstallError::IndexReadError(_, msg) => format!("Failed to read plugin index: {}", msg),
        InstallError::IndexParseError(_, msg) => format!("Failed to parse plugin index: {}", msg),
        InstallError::Network(e) => format!("Network error: {}", e.to_string()),
        InstallError::Io(e) => format!("IO error: {}", e.to_string()),
        _ => e.to_string(),
    }
}
