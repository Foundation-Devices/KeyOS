// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Sideload command - build, sign, upload, and optionally launch an app on hardware over USB.

use std::path::Path;

use anyhow::{Context, Result};
use foundation_core::ProjectContext;
use foundation_mcp::PassportDriveMcpClient;

use crate::assets::BUNDLED_ICON_FILE;
use crate::commands::build;

pub fn execute(release: bool, no_run: bool) -> Result<()> {
    println!("Building and sideloading the application...");
    build::execute(release)?;

    let project = ProjectContext::discover().context("app-config.toml not found")?;
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
    mcp.is_developer_mode_enabled().map_err(|error| {
        anyhow::anyhow!("Developer Mode must be enabled before sideloading. On Passport Prime, open Settings > Apps and turn on Developer Mode, then reconnect USB and try again: {error}")
    })?;
    println!("passport-drive MCP control connected.");

    println!("Uploading signed app bundle via usb-debug...");
    let load_response = mcp.load_app(&artifact_dir).map_err(|error| {
        anyhow::anyhow!(
            "Could not upload {} over usb-debug. Make sure Developer Mode is enabled and no other process is using the Passport USB debug interface. Reason: {}",
            artifact_dir.display(),
            error
        )
    })?;
    println!("{load_response}");

    if no_run {
        println!("Skipping automatic launch (--no-run).");
        println!("Sideload complete!");
        return Ok(());
    }

    println!("Launching {} via passport-drive MCP...", config.app_name);
    let app_id_slice = config.app_id.as_bytes();
    let app_id_bytes: &[u8; 16] = app_id_slice.try_into().context("App ID must be exactly 16 bytes")?;
    let launch_response = mcp.launch_app(app_id_bytes).map_err(|error| {
        anyhow::anyhow!(
            "The app was uploaded, but launching it through passport-drive MCP failed. Make sure Developer Mode is enabled. Reason: {}",
            error
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
