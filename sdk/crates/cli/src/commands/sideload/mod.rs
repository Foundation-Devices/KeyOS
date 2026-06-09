// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Sideload command - build, sign, copy, and optionally launch an app on hardware over USB.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use foundation_core::{AppId, ProjectContext};
use foundation_mcp::PassportDriveMcpClient;

use crate::assets::{copy_app_resources_to_bundle, BUNDLED_ICON_FILE};
use crate::commands::build;

const SIDELOADED_APPS_DIR: &str = "sideloaded-apps";

pub fn execute(
    release: bool,
    no_run: bool,
    mount_path: Option<&str>,
    _serial_port: Option<&str>,
) -> Result<()> {
    println!("Building and sideloading the application...");
    build::execute(release)?;

    let project = ProjectContext::discover().context("app-config.toml not found")?;
    let config = &project.config;
    let artifact_dir = project.root.join("target").join("keyos").join(&config.app_name);
    let app_elf = artifact_dir.join("app.elf");
    let manifest = artifact_dir.join("manifest.json");
    let icon = artifact_dir.join(BUNDLED_ICON_FILE);
    let app_resources = artifact_dir.join("resources");
    ensure_exists(&app_elf)?;
    ensure_exists(&manifest)?;
    ensure_exists(&icon)?;

    println!("Resolving Passport Prime USB storage mount...");
    let mount_root = resolve_mount_root(mount_path)?;
    println!("Using device mount: {}", mount_root.display());

    println!("Checking passport-drive MCP control...");
    let mut mcp = PassportDriveMcpClient::connect().map_err(|error| {
        anyhow::anyhow!("Could not start passport-drive MCP control. Make sure Passport Prime is unlocked, connected by USB, and Developer Mode is enabled.: {error}")
    })?;
    mcp.is_developer_mode_enabled().map_err(|error| {
        anyhow::anyhow!("Developer Mode must be enabled before sideloading. On Passport Prime, open Settings > Apps and turn on Developer Mode, then reconnect USB and try again.: {error}")
    })?;
    println!("passport-drive MCP control connected.");

    println!("Copying signed app bundle to the device...");
    let install_dir = install_sideloaded_apps_root(&mount_root).join(sideload_app_dir_name(&config.app_id));
    fs::create_dir_all(&install_dir).with_context(|| {
        format!(
            "Failed to create app directory {}. Make sure Passport Prime is unlocked, connected by USB, and Airlock is mounted in Read/Write mode.",
            install_dir.display()
        )
    })?;
    copy_with_sync(&app_elf, &install_dir.join("app.elf"))?;
    copy_with_sync(&manifest, &install_dir.join("manifest.json"))?;
    copy_with_sync(&icon, &install_dir.join(BUNDLED_ICON_FILE))?;
    let install_resources = install_dir.join("resources");
    copy_app_resources_to_bundle(&app_resources, &install_dir).with_context(|| {
        format!(
            "Failed to copy app assets to {}. Make sure Passport Prime is unlocked, connected by USB, and Airlock is mounted in Read/Write mode.",
            install_resources.display()
        )
    })?;
    sync_dir_if_possible(&install_dir)?;
    sync_dir_if_possible(&install_resources)?;

    println!("Copied app bundle to {}", install_dir.display());

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
            "The app was copied to {}, but launching it through passport-drive MCP failed. Make sure Developer Mode is enabled. Reason: {}",
            install_dir.display(),
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

fn resolve_mount_root(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        let path = PathBuf::from(path);
        if path.is_dir() {
            return Ok(path);
        }
        anyhow::bail!("Mount path does not exist or is not a directory: {}", path.display());
    }

    let username = std::env::var("USER").ok().or_else(|| std::env::var("LOGNAME").ok());
    let mut candidates = vec![PathBuf::from("/Volumes/PRIME"), PathBuf::from("/mnt/PRIME")];
    if let Some(username) = username {
        candidates.push(PathBuf::from(format!("/media/{username}/PRIME")));
        candidates.push(PathBuf::from(format!("/run/media/{username}/PRIME")));
    }
    candidates.push(PathBuf::from("/media/PRIME"));

    if let Some(path) = candidates.iter().find(|path| path.is_dir()) {
        return Ok(path.clone());
    }

    let candidate_list = candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
    anyhow::bail!(
        "Could not find the mounted PRIME USB volume at any of: {}. Connect the device over USB and make sure USB storage is enabled.",
        candidate_list
    );
}

fn install_sideloaded_apps_root(mount_root: &Path) -> PathBuf {
    let nested_keyos_root = mount_root.join("keyos");
    if nested_keyos_root.is_dir() {
        return nested_keyos_root.join(SIDELOADED_APPS_DIR);
    }

    if mount_root.file_name().and_then(|name| name.to_str()) == Some("keyos")
        || mount_root.join("apps").is_dir()
        || mount_root.join(SIDELOADED_APPS_DIR).is_dir()
    {
        return mount_root.join(SIDELOADED_APPS_DIR);
    }

    nested_keyos_root.join(SIDELOADED_APPS_DIR)
}

fn sideload_app_dir_name(app_id: &AppId) -> &str {
    app_id.as_hex().strip_prefix("0x").expect("AppId stores normalized 0x-prefixed hex")
}

fn copy_with_sync(source: &Path, destination: &Path) -> Result<()> {
    fs::copy(source, destination).with_context(|| copy_failure(source, destination))?;
    // Open the copy writable before sync_all(): on Windows a read-only handle makes
    // sync_all() (a flush) fail, so the sync must hold a writable handle.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)
        .with_context(|| copy_failure(source, destination))?;
    file.sync_all().with_context(|| copy_failure(source, destination))?;
    Ok(())
}

fn copy_failure(source: &Path, destination: &Path) -> String {
    format!(
        "Failed to copy {} to {}. Make sure Passport Prime is unlocked, connected by USB, and Airlock is mounted in Read/Write mode.",
        source.display(),
        destination.display()
    )
}

fn sync_dir_if_possible(path: &Path) -> Result<()> {
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use foundation_core::AppId;

    use super::{install_sideloaded_apps_root, resolve_mount_root, sideload_app_dir_name};
    use crate::test_support::PROCESS_LOCK;

    #[test]
    fn explicit_mount_path_is_accepted() {
        let _guard = PROCESS_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join("foundation-sideload-mount-test");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(resolve_mount_root(Some(root.to_str().unwrap())).unwrap(), root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_sideloaded_apps_root_uses_keyos_subdirectory_from_volume_root() {
        assert_eq!(
            install_sideloaded_apps_root(Path::new("/Volumes/PRIME")),
            Path::new("/Volumes/PRIME/keyos/sideloaded-apps")
        );
    }

    #[test]
    fn install_sideloaded_apps_root_accepts_keyos_directory_as_mount_root() {
        assert_eq!(
            install_sideloaded_apps_root(Path::new("/Volumes/PRIME/keyos")),
            Path::new("/Volumes/PRIME/keyos/sideloaded-apps")
        );
    }

    #[test]
    fn install_sideloaded_apps_root_detects_direct_keyos_directory_from_apps_directory() {
        let _guard = PROCESS_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join("foundation-sideload-direct-apps-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("apps")).unwrap();
        assert_eq!(install_sideloaded_apps_root(&root), root.join("sideloaded-apps"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_sideloaded_apps_root_prefers_nested_keyos_directory_when_present() {
        let _guard = PROCESS_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join("foundation-sideload-nested-keyos-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::create_dir_all(root.join("keyos")).unwrap();
        assert_eq!(install_sideloaded_apps_root(&root), root.join("keyos").join("sideloaded-apps"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sideload_app_dir_name_uses_lowercase_app_id_without_prefix() {
        let app_id = AppId::from_hex("0xAABBCCDDEEFF00112233445566778899").unwrap();

        assert_eq!(sideload_app_dir_name(&app_id), "aabbccddeeff00112233445566778899");
    }
}
