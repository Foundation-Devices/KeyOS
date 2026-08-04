// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Project-local Foundation SDK path mapping.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use foundation_core::{SdkLayout, SdkRoot};

const PROJECT_SDK_DIR: &str = ".foundation-sdk";
const PROJECT_SDK_CURRENT: &str = "current";
const PROJECT_SDK_KEYOS_ROOT: &str = ".foundation-sdk/current/lib/keyos";
const PROJECT_SDK_ROOT: &str = ".foundation-sdk/current";
const PROJECT_SDK_UI_ROOT: &str = ".foundation-sdk/current/ui/ui";

pub fn project_sdk_dir(project_root: &Path) -> PathBuf { project_root.join(PROJECT_SDK_DIR) }

pub fn project_sdk_root_path() -> &'static str { PROJECT_SDK_ROOT }

pub fn project_sdk_keyos_root_path() -> &'static str { PROJECT_SDK_KEYOS_ROOT }

pub fn project_sdk_ui_root_path() -> &'static str { PROJECT_SDK_UI_ROOT }

pub fn ensure_project_sdk_mapping(project_root: &Path, sdk: &SdkRoot) -> Result<()> {
    let sdk_dir = project_sdk_dir(project_root);
    ensure_managed_sdk_dir(&sdk_dir)?;

    let current = sdk_dir.join(PROJECT_SDK_CURRENT);
    match sdk.layout() {
        SdkLayout::Bundle => ensure_bundle_mapping(&current, sdk),
        SdkLayout::Repo => ensure_repo_facade(&current, sdk),
    }
}

fn ensure_managed_sdk_dir(sdk_dir: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(sdk_dir) {
        if metadata.file_type().is_symlink() {
            fs::remove_file(sdk_dir).with_context(|| format!("Failed to replace {}", sdk_dir.display()))?;
        } else if !metadata.is_dir() {
            anyhow::bail!("Expected {} to be a directory or symlink", sdk_dir.display());
        }
    }

    fs::create_dir_all(sdk_dir).with_context(|| format!("Failed to create {}", sdk_dir.display()))
}

fn ensure_bundle_mapping(current: &Path, sdk: &SdkRoot) -> Result<()> {
    replace_with_symlink(&preferred_bundle_current_link(sdk), current)
}

fn preferred_bundle_current_link(sdk: &SdkRoot) -> PathBuf {
    let sdk_root = sdk.root();
    let Some(parent) = sdk_root.parent() else {
        return sdk_root.to_path_buf();
    };

    let current = parent.join(PROJECT_SDK_CURRENT);
    if !current.exists() {
        return sdk_root.to_path_buf();
    }

    let Ok(resolved_current) = fs::canonicalize(&current) else {
        return sdk_root.to_path_buf();
    };
    let resolved_root = fs::canonicalize(sdk_root).unwrap_or_else(|_| sdk_root.to_path_buf());
    if resolved_current == resolved_root {
        current
    } else {
        sdk_root.to_path_buf()
    }
}

fn ensure_repo_facade(current: &Path, sdk: &SdkRoot) -> Result<()> {
    remove_existing(current)?;
    fs::create_dir_all(current.join("lib"))
        .with_context(|| format!("Failed to create {}", current.join("lib").display()))?;
    fs::create_dir_all(current.join("ui"))
        .with_context(|| format!("Failed to create {}", current.join("ui").display()))?;

    create_dir_symlink(&sdk.keyos_root(), &current.join("lib").join("keyos")).with_context(|| {
        format!(
            "Failed to map SDK KeyOS sources from {} into {}",
            sdk.keyos_root().display(),
            current.join("lib").join("keyos").display()
        )
    })?;
    create_dir_symlink(&sdk.ui_library_path(), &current.join("ui").join("ui")).with_context(|| {
        format!(
            "Failed to map SDK UI sources from {} into {}",
            sdk.ui_library_path().display(),
            current.join("ui").join("ui").display()
        )
    })?;
    create_dir_symlink(&sdk.ui_shared_resources_path(), &current.join("resources")).with_context(|| {
        format!(
            "Failed to map SDK shared resources from {} into {}",
            sdk.ui_shared_resources_path().display(),
            current.join("resources").display()
        )
    })?;

    Ok(())
}

fn replace_with_symlink(source: &Path, target: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(target) {
        if metadata.file_type().is_symlink() {
            if fs::read_link(target).ok().as_deref() == Some(source) {
                return Ok(());
            }
        }
        remove_existing(target)?;
    }

    create_dir_symlink(source, target)
        .with_context(|| format!("Failed to create SDK symlink {} -> {}", target.display(), source.display()))
}

fn remove_existing(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path).with_context(|| format!("Failed to remove {}", path.display()))?;
    } else {
        fs::remove_dir_all(path).with_context(|| format!("Failed to remove {}", path.display()))?;
    }

    Ok(())
}

#[cfg(unix)]
fn create_dir_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn create_dir_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(source, target)
}

#[cfg(not(any(unix, windows)))]
fn create_dir_symlink(_source: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory symlinks are not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use foundation_core::SdkRoot;

    use super::{ensure_project_sdk_mapping, project_sdk_keyos_root_path};
    use crate::test_support::make_temp_dir;

    #[test]
    fn bundle_mapping_targets_installed_current_link_when_available() {
        let install_root_dir = make_temp_dir("installed-sdk");
        let install_root = install_root_dir.path();
        let bundle_root = install_root.join("foundation-sdk-1.0.0-x86_64-unknown-linux-gnu");
        make_bundle_root(&bundle_root);
        let current = install_root.join("current");
        create_dir_symlink(&bundle_root, &current).unwrap();

        let sdk = SdkRoot::from_root(current.clone()).unwrap();
        let project_root_dir = make_temp_dir("project");
        let project_root = project_root_dir.path();

        ensure_project_sdk_mapping(project_root, &sdk).unwrap();

        let project_current = project_root.join(".foundation-sdk").join("current");
        let normalized_current = fs::canonicalize(install_root).unwrap().join("current");
        assert_eq!(fs::read_link(&project_current).unwrap(), normalized_current);
        assert!(project_root.join(project_sdk_keyos_root_path()).exists());
    }

    #[test]
    fn repo_mapping_builds_bundle_shaped_facade() {
        let (_repo_dir, sdk_root) = make_repo_root("repo-sdk");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let project_root_dir = make_temp_dir("project");
        let project_root = project_root_dir.path();

        ensure_project_sdk_mapping(project_root, &sdk).unwrap();

        assert!(project_root.join(project_sdk_keyos_root_path()).exists());
        assert!(project_root.join(".foundation-sdk").join("current").join("ui").join("ui").exists());
        assert!(project_root.join(".foundation-sdk").join("current").join("resources").exists());
    }

    fn make_bundle_root(root: &Path) {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("flake.nix"), "{}").unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("lib").join("keyos")).unwrap();
        fs::create_dir_all(root.join("ui").join("ui")).unwrap();
        fs::create_dir_all(root.join("resources")).unwrap();
    }

    fn make_repo_root(label: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = make_temp_dir(label);
        let repo_root = dir.path();
        fs::write(repo_root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let root = repo_root.join("sdk");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("flake.nix"), "{}").unwrap();
        fs::write(root.join("sdk-build.toml"), "").unwrap();
        fs::create_dir_all(repo_root.join("ui2").join("components").join("ui")).unwrap();
        fs::create_dir_all(repo_root.join("ui2").join("resources")).unwrap();
        (dir, root)
    }

    #[cfg(unix)]
    fn create_dir_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(source, target)
    }

    #[cfg(windows)]
    fn create_dir_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(source, target)
    }
}
