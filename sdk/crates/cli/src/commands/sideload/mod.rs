// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Sideload command - build, sign, upload, and optionally launch an app on hardware over USB.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use clap::Args;
use foundation_core::ProjectContext;
use foundation_mcp::PassportDriveMcpClient;
use foundation_ui::TerminalUI;

use crate::assets::BUNDLED_ICON_FILE;
use crate::commands::build::{self, BuildArgs};

#[derive(Args)]
pub struct SideloadArgs {
    /// Build in release mode with optimizations before sideloading
    #[arg(short, long)]
    pub release: bool,

    /// Upload the app bundle to the device but do not launch it afterward
    #[arg(long)]
    pub no_run: bool,
}

pub fn execute(args: &SideloadArgs) -> Result<()> {
    let no_run = args.no_run;

    println!("Building and sideloading the application...");
    build::execute(&BuildArgs { release: args.release })?;

    let project = ProjectContext::discover()?;
    let config = &project.config;
    let artifact_dir = project.root.join("target").join("keyos").join(&config.app_name);
    let app_elf = artifact_dir.join("app.elf");
    let manifest = artifact_dir.join("manifest.json");
    let icon = artifact_dir.join(BUNDLED_ICON_FILE);
    ensure_exists(&app_elf)?;
    ensure_exists(&manifest)?;
    ensure_exists(&icon)?;

    println!("Checking passport-drive MCP control...");
    let mut mcp = PassportDriveMcpClient::connect().map_err(|error| {
        anyhow::anyhow!("Could not start passport-drive MCP control. Make sure Passport Prime is unlocked, connected by USB, and Developer Mode is enabled: {error}")
    })?;
    println!("passport-drive MCP control connected.");

    let ui = TerminalUI::new();
    let upload_message = match directory_size_bytes(&artifact_dir) {
        Ok(bytes) => format!("Uploading signed app bundle via usb-debug ({})...", format_bytes(bytes)),
        Err(_) => "Uploading signed app bundle via usb-debug...".to_string(),
    };
    let upload_spinner = ui.spinner(&upload_message);
    let load_response = match mcp.load_app(&artifact_dir) {
        Ok(response) => {
            upload_spinner.finish_success("Upload complete");
            response
        }
        Err(error) => {
            upload_spinner.finish_clear();
            return Err(anyhow::anyhow!(
                "Could not upload {} over usb-debug. Make sure Developer Mode is enabled and no other process is using the Passport USB debug interface. Reason: {}",
                artifact_dir.display(),
                error
            ));
        }
    };
    println!("{load_response}");

    if no_run {
        println!("Skipping automatic launch (--no-run).");
        println!("Sideload complete!");
        return Ok(());
    }

    let preflight_spinner = ui.spinner("Checking allowed publisher certificate...");
    if let Err(error) = mcp.ensure_allowed_publisher_installed() {
        preflight_spinner.finish_clear();
        return Err(anyhow::anyhow!(
            "Sideload preflight failed before launch. On Passport Prime, import the matching certificate in Settings > Apps > Allowed Publishers, then reconnect USB and try again. Reason: {error}"
        ));
    }
    preflight_spinner.finish_success("Allowed publisher certificate found");

    println!("Launching {} via passport-drive MCP...", config.app_name);
    let app_id_slice = config.app_id.as_bytes();
    let app_id_bytes: &[u8; 16] = app_id_slice.try_into().context("App ID must be exactly 16 bytes")?;
    let launch_response = mcp.launch_app(app_id_bytes).map_err(|error| {
        let message = launch_error_message(&error.to_string());
        anyhow::anyhow!(
            "The app was uploaded, but launching it through passport-drive MCP failed. {}",
            message
        )
    })?;
    println!("Launch request accepted: {}", launch_response);
    println!("Sideload complete!");

    Ok(())
}

fn ensure_exists(path: &Path) -> Result<()> {
    if path.exists() {
        Ok(())
    } else {
        anyhow::bail!("Expected build artifact is missing: {}", path.display());
    }
}

fn directory_size_bytes(path: &Path) -> Result<u64> {
    let metadata = fs::metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }

    let mut total = 0u64;
    for entry in fs::read_dir(path).with_context(|| format!("Failed to read {}", path.display()))? {
        let entry = entry?;
        total = total.saturating_add(directory_size_bytes(&entry.path())?);
    }
    Ok(total)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];

    for next_unit in UNITS.iter().skip(1) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = *next_unit;
    }

    if unit == "B" {
        format!("{bytes} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn launch_error_message(error: &str) -> String {
    let message = error.strip_prefix("Error: ").unwrap_or(error);
    if message.contains("device returned status 0x01") {
        format!(
            "{message}. The upload completed, so Developer Mode is reachable; this is a generic launch rejection from firmware or helper tooling that did not report a detailed reason. For sideloaded apps, the most likely cause is that no matching allowed publisher certificate is installed in Settings > Apps > Allowed Publishers."
        )
    } else {
        message.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::launch_error_message;

    #[test]
    fn launch_error_message_does_not_blame_developer_mode_for_legacy_generic_status() {
        let message = launch_error_message("Error: Failed to launch app: device returned status 0x01");

        assert!(message.contains("Developer Mode is reachable"));
        assert!(message.contains("matching allowed publisher certificate"));
    }
}
