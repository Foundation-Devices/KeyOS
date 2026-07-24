// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `foundation theme` - open the app theme in the visual theme editor.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::Args;
use foundation_core::{ProjectContext, SdkRoot, APP_CONFIG_FILE};

const DEFAULT_APP_THEME_PATH: &str = "resources/theme.json";

#[derive(Args)]
pub struct ThemeArgs {
    /// Theme JSON to edit; may be any relative or absolute path
    #[arg(value_name = "FILENAME")]
    pub filename: Option<PathBuf>,
}

pub fn execute(args: &ThemeArgs) -> Result<()> {
    let sdk = SdkRoot::discover().context(
        "Could not locate the Foundation SDK root. Run from an SDK checkout or unpacked bundle, or set FOUNDATION_SDK_ROOT.",
    )?;

    if let Some(filename) = &args.filename {
        let current_dir = std::env::current_dir().context("failed to determine the current directory")?;
        let theme_path = if filename.is_absolute() { filename.clone() } else { current_dir.join(filename) };
        let editor_dir = theme_path.parent().filter(|parent| parent.is_dir()).unwrap_or(&current_dir);
        return launch_theme_editor(&sdk, editor_dir, &theme_path);
    }

    let project = ProjectContext::discover().context("app-config.toml not found")?;
    let project_root = project.root.as_path();
    let config = &project.config;

    let configured_theme = config.theme.as_deref().map(str::trim).filter(|theme| !theme.is_empty());
    let theme_path = match configured_theme {
        Some(theme) if crate::commands::themes::is_theme_path(theme) => {
            let path = crate::commands::themes::resolve_project_path(project_root, theme);
            if !path.exists() {
                crate::commands::themes::write_editable_app_theme(
                    theme,
                    &sdk,
                    project_root,
                    &path,
                    &config.friendly_app_name,
                    None,
                )?;
            } else {
                crate::commands::themes::ensure_editable_app_theme_parent(&path)?;
            }
            path
        }
        Some(theme) => {
            let path = project_root.join(DEFAULT_APP_THEME_PATH);
            crate::commands::themes::write_editable_app_theme(
                theme,
                &sdk,
                project_root,
                &path,
                &config.friendly_app_name,
                None,
            )?;
            set_app_config_theme(project_root, DEFAULT_APP_THEME_PATH)?;
            path
        }
        None => {
            let path = project_root.join(DEFAULT_APP_THEME_PATH);
            crate::commands::themes::write_editable_app_theme(
                "base_theme",
                &sdk,
                project_root,
                &path,
                &config.friendly_app_name,
                None,
            )?;
            set_app_config_theme(project_root, DEFAULT_APP_THEME_PATH)?;
            path
        }
    };

    launch_theme_editor(&sdk, project_root, &theme_path)
}

fn launch_theme_editor(sdk: &SdkRoot, project_root: &Path, theme_path: &Path) -> Result<()> {
    let theme_path = fs::canonicalize(theme_path).unwrap_or_else(|_| theme_path.to_path_buf());

    println!("Opening theme editor for {}...", theme_path.display());

    let status = if let Some(editor) = sdk.tool_path(&["foundation-theme-editor", "theme-editor"]) {
        Command::new(editor).arg(&theme_path).current_dir(project_root).status()
    } else {
        let manifest = sdk.keyos_root().join("ui2").join("theme-editor").join("Cargo.toml");
        if !manifest.exists() {
            anyhow::bail!("Could not find the theme editor. Reinstall the Foundation SDK.");
        }
        Command::new("cargo")
            .arg("run")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("--bin")
            .arg("theme-editor")
            .arg("--")
            .arg(&theme_path)
            .current_dir(project_root)
            .status()
    }
    .context("Failed to start theme editor")?;

    if !status.success() {
        anyhow::bail!("Theme editor exited with {}", status);
    }

    Ok(())
}

fn set_app_config_theme(project_root: &Path, theme: &str) -> Result<()> {
    let path = project_root.join(APP_CONFIG_FILE);
    let content = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let theme_line = format!("theme = {}", toml_string(theme));
    let mut output = Vec::new();
    let mut replaced = false;
    let mut inserted = false;
    let mut in_top_level = true;

    for line in content.lines() {
        let trimmed = line.trim_start();
        if in_top_level && is_top_level_theme_line(trimmed) {
            output.push(theme_line.clone());
            replaced = true;
            continue;
        }

        if in_top_level && !replaced && !inserted && trimmed.starts_with('[') {
            output.push(theme_line.clone());
            output.push(String::new());
            inserted = true;
            in_top_level = false;
        } else if trimmed.starts_with('[') {
            in_top_level = false;
        }

        output.push(line.to_string());
    }

    if !replaced && !inserted {
        if output.last().map(|line| !line.trim().is_empty()).unwrap_or(false) {
            output.push(String::new());
        }
        output.push(theme_line);
    }

    fs::write(&path, format!("{}\n", output.join("\n")))
        .with_context(|| format!("failed to write {}", path.display()))
}

fn is_top_level_theme_line(trimmed: &str) -> bool {
    trimmed.starts_with("theme") && trimmed["theme".len()..].trim_start().starts_with('=')
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"resources/theme.json\"".to_string())
}
