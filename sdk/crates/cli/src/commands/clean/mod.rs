// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `foundation clean` - remove generated app build and theme artifacts.
//!
//! Deletes the reproducible files an app build/sim/theme run generates inside the
//! project, leaving authored source untouched:
//!
//! - `target/` - cargo output plus the generated `target/foundation/**` UI, resources, and sim-resources
//!   trees, the generated theme `target/foundation/themes/{json,rust,slint}` files, and the
//!   `target/keyos/<app-name>/**` hardware bundles.
//! - `manifest.toml` - the compatibility manifest written before each cargo build.
//! - `ui/ui` - the SDK UI library mapping (symlink or copied snapshot).
//!
//! Authored content under `resources/` (the app icon, theme JSON, images, and
//! fonts) is never touched; builds copy it into `target/foundation/resources/`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use foundation_core::APP_CONFIG_FILE;

/// Execute the `foundation clean` command.
pub fn execute() -> Result<()> {
    let cwd = std::env::current_dir().context("Could not determine the current directory")?;
    let project_root = find_project_root(&cwd).context(
        "app-config.toml not found. Run 'foundation clean' from inside a Foundation app directory.",
    )?;

    println!("Cleaning generated build and theme files...");
    println!("Project: {}", project_root.display());
    println!();

    clean_project(&project_root)
}

/// Walk up from `start` to the app project root (the directory containing
/// `app-config.toml`). Unlike [`ProjectContext::discover`], this neither parses
/// nor validates the config, so `clean` still works when the config is mid-edit
/// or the build is broken - exactly when removing generated files is most useful.
///
/// [`ProjectContext::discover`]: foundation_core::ProjectContext::discover
fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(APP_CONFIG_FILE).exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Remove the generated artifacts under `project_root`, reporting each step.
fn clean_project(project_root: &Path) -> Result<()> {
    let mut removed_any = false;

    // `target/` holds all cargo output plus the generated `target/foundation/**`
    // trees (ui, resources, sim-resources, themes json/rust/slint) and the
    // `target/keyos/**` hardware bundles - every generated build and theme file
    // lives under here.
    removed_any |= remove_path(&project_root.join("target"), "target/")?;

    // Generated compatibility manifest written before each cargo build.
    removed_any |= remove_path(&project_root.join("manifest.toml"), "manifest.toml")?;

    // Snapshot/symlink of the SDK UI library materialized into the app tree.
    removed_any |= remove_path(&project_root.join("ui").join("ui"), "ui/ui")?;

    println!();
    if removed_any {
        println!("Clean complete. Run 'foundation build' or 'foundation sim' to regenerate.");
    } else {
        println!("Nothing to clean - no generated files were found.");
    }

    Ok(())
}

/// Remove a generated file or directory if present, reporting the outcome.
///
/// Uses `symlink_metadata` so a symlinked `ui/ui` mapping is removed as a link
/// rather than recursing into the SDK source it points at. Output goes through
/// plain `println!` so the removal summary is visible in non-interactive runs
/// (piped output, CI, agents), matching how `foundation build` reports steps.
fn remove_path(path: &Path, label: &str) -> Result<bool> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(false);
    };

    if metadata.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("Failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("Failed to remove {}", path.display()))?;
    }

    println!("  Removed {label}");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{clean_project, find_project_root};

    #[test]
    fn finds_project_root_from_nested_dir_without_parsing_config() {
        let project_root = make_temp_dir("clean-find-root");
        // A deliberately incomplete (unparseable as a full AppConfig) file: clean
        // only needs the marker file to exist, not a valid config.
        fs::write(project_root.join("app-config.toml"), "app-name = \"demo\"\n").unwrap();
        let nested = project_root.join("ui").join("pages");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_project_root(&nested).as_deref(), Some(project_root.as_path()));

        let no_root = make_temp_dir("clean-no-root");
        assert!(find_project_root(&no_root).is_none());

        cleanup(&no_root);
        cleanup(&project_root);
    }

    #[test]
    fn removes_generated_build_and_theme_artifacts() {
        let project_root = make_temp_dir("clean-generated");

        // Generated build + theme tree under target/.
        let themes = project_root.join("target").join("foundation").join("themes").join("rust");
        fs::create_dir_all(&themes).unwrap();
        fs::write(themes.join("mod.rs"), "// generated\n").unwrap();
        let keyos = project_root.join("target").join("keyos").join("demo-app");
        fs::create_dir_all(&keyos).unwrap();
        fs::write(keyos.join("app.elf"), b"elf").unwrap();

        // Other generated mapping/manifest files.
        fs::create_dir_all(project_root.join("ui").join("ui")).unwrap();
        fs::write(project_root.join("ui").join("ui").join("theme.slint"), "theme\n").unwrap();
        fs::write(project_root.join("manifest.toml"), "appId = \"0x0\"\n").unwrap();

        // Authored source that must survive a clean.
        fs::write(project_root.join("app-config.toml"), "app-name = \"demo-app\"\n").unwrap();
        fs::create_dir_all(project_root.join("ui")).unwrap();
        fs::write(project_root.join("ui").join("app.slint"), "export component App {}\n").unwrap();
        fs::create_dir_all(project_root.join("resources").join("images")).unwrap();
        fs::write(project_root.join("resources").join("images").join("logo.svg"), "<svg/>\n").unwrap();

        clean_project(&project_root).unwrap();

        // Generated artifacts are gone.
        assert!(!project_root.join("target").exists());
        assert!(!project_root.join("manifest.toml").exists());
        assert!(!project_root.join("ui").join("ui").exists());

        // Authored source is untouched.
        assert!(project_root.join("app-config.toml").exists());
        assert!(project_root.join("ui").join("app.slint").exists());
        assert!(project_root.join("resources").join("images").join("logo.svg").exists());

        cleanup(&project_root);
    }

    #[test]
    fn never_touches_authored_resources() {
        let project_root = make_temp_dir("clean-resources");
        let resources = project_root.join("resources");

        // Authored resource content: icon, theme JSON, and asset directories.
        fs::create_dir_all(resources.join("fonts")).unwrap();
        fs::create_dir_all(resources.join("images")).unwrap();
        fs::write(resources.join("icon.svg"), "<svg/>\n").unwrap();
        fs::write(resources.join("theme.json"), "{}\n").unwrap();
        fs::write(resources.join("fonts").join("Brand.ttf"), b"font").unwrap();
        fs::write(resources.join("images").join("logo.svg"), "<svg/>\n").unwrap();

        clean_project(&project_root).unwrap();

        assert!(resources.join("icon.svg").exists());
        assert!(resources.join("theme.json").exists());
        assert!(resources.join("fonts").join("Brand.ttf").exists());
        assert!(resources.join("images").join("logo.svg").exists());

        cleanup(&project_root);
    }

    #[test]
    fn clean_is_idempotent_when_nothing_generated() {
        let project_root = make_temp_dir("clean-empty");
        // No target/, manifest.toml, or ui/ui present.
        clean_project(&project_root).unwrap();
        cleanup(&project_root);
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("foundation-clean-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn cleanup(path: &Path) { let _ = fs::remove_dir_all(path); }
}
