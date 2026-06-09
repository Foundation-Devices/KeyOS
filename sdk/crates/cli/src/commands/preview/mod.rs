// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Preview command - run the Foundation Slint viewer on a Slint file

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use clap::ArgMatches;
use foundation_core::SdkRoot;

use crate::slint_codegen::{find_project_root, prepare_slint_file_for_view, project_sdk_ui_root};

/// Default Slint file to preview
const DEFAULT_SLINT_FILE: &str = "ui/app.slint";

/// Execute the preview command
pub fn execute(matches: &ArgMatches) -> Result<()> {
    // Determine the file to preview
    let slint_file = match matches.get_one::<String>("file").map(|s| s.as_str()) {
        Some(f) => PathBuf::from(f),
        None => {
            println!("Using default file: ui/app.slint");
            PathBuf::from(DEFAULT_SLINT_FILE)
        }
    };

    // Check if file exists
    if !slint_file.exists() {
        anyhow::bail!("Slint file not found: {}", slint_file.display());
    }

    let sdk = SdkRoot::discover().ok();
    let viewer = sdk
        .as_ref()
        .and_then(|sdk| sdk.tool_path(&["foundation-slint-viewer", "slint-viewer"]))
        .or_else(|| find_in_path(&["foundation-slint-viewer", "slint-viewer"]))
        .ok_or_else(|| anyhow::anyhow!("foundation-slint-viewer not found. Make sure you're in the Foundation development environment (run 'foundation develop')."))?;

    prepare_slint_file_for_view(&slint_file, sdk.as_ref())?;

    println!("Previewing {} with foundation-slint-viewer...", slint_file.display());
    println!();

    // Run the Foundation viewer (or the plain viewer alias when necessary)
    let mut cmd = Command::new(viewer);

    // Resolve the project root once; both the `@ui` and `@theme` library maps need it.
    let project_root = find_project_root(&slint_file);

    if let Some(sdk) = sdk.as_ref() {
        let project_ui_lib_path = project_root.as_deref().map(project_sdk_ui_root);
        let sdk_ui_lib_path = sdk.ui_library_path();
        let ui_lib_path =
            project_ui_lib_path.as_deref().filter(|path| path.exists()).unwrap_or(&sdk_ui_lib_path);
        if ui_lib_path.exists() {
            cmd.arg("-L").arg(format!("ui={}", ui_lib_path.display()));
        }
    }

    // `@theme` namespace → the per-app generated component themes
    // (button_theme.slint, …), the same directory `foundation build`/`sim` point
    // the Slint compiler at via FOUNDATION_THEMES_SLINT_DIR. Without this the
    // viewer can't resolve `import … from "@theme/button_theme.slint"`.
    if let Some(theme_lib_path) =
        project_root.as_deref().map(crate::commands::themes::project_theme_slint_dir)
    {
        if theme_lib_path.exists() {
            cmd.arg("-L").arg(format!("theme={}", theme_lib_path.display()));
        }
    }

    if let Some(values) = matches.get_many::<String>("include-path") {
        for val in values {
            cmd.arg("-I").arg(val);
        }
    }
    if let Some(val) = matches.get_one::<String>("style") {
        cmd.arg("--style").arg(val);
    }
    if let Some(val) = matches.get_one::<String>("component") {
        cmd.arg("--component").arg(val);
    }
    if let Some(val) = matches.get_one::<String>("backend") {
        cmd.arg("--backend").arg(val);
    }
    if matches.get_flag("auto-reload") {
        cmd.arg("--auto-reload");
    }
    if let Some(val) = matches.get_one::<String>("load-data") {
        cmd.arg("--load-data").arg(val);
    }
    if let Some(val) = matches.get_one::<String>("save-data") {
        cmd.arg("--save-data").arg(val);
    }
    if let Some(values) = matches.get_many::<String>("on") {
        let vals: Vec<&String> = values.collect();
        for pair in vals.chunks(2) {
            cmd.arg("--on").arg(pair[0]).arg(pair[1]);
        }
    }
    if let Some(val) = matches.get_one::<String>("i18n-dir") {
        cmd.arg("--i18n-dir").arg(val);
    }
    if let Some(val) = matches.get_one::<String>("locale") {
        cmd.arg("--locale").arg(val);
    }

    let status = cmd.arg(&slint_file).status().context("Failed to start foundation-slint-viewer")?;

    if !status.success() {
        anyhow::bail!("Failed to start foundation-slint-viewer");
    }

    Ok(())
}

fn find_in_path(commands: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for command in commands {
            let candidate = dir.join(command);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
