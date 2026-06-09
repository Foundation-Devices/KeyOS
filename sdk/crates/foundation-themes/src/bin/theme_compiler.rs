// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `foundation-theme-compiler` — host-side codegen tool.
//!
//! Reads theme JSON from a source directory and writes generated Rust
//! (one `<id>.rs` per theme plus a `mod.rs` index) to an output directory.
//! The `foundation themes build` CLI command shells out to this so the lean
//! host CLI doesn't have to link `components`/`slint`.
//!
//! ```text
//! foundation-theme-compiler --json-dir <dir> --rust-dir <dir>
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(ids) => {
            for id in ids {
                println!("{id}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("foundation-theme-compiler: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<Vec<String>> {
    let mut json_dir: Option<PathBuf> = None;
    let mut rust_dir: Option<PathBuf> = None;
    // Optional: also emit per-app component theme `.slint` files.
    let mut plugin_dir: Option<PathBuf> = None;
    let mut slint_dir: Option<PathBuf> = None;
    let mut app_theme_json: Option<PathBuf> = None;
    let mut components: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json-dir" => {
                json_dir = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow::anyhow!("--json-dir requires a path"))?,
                ));
            }
            "--rust-dir" => {
                rust_dir = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow::anyhow!("--rust-dir requires a path"))?,
                ));
            }
            "--plugin-dir" => {
                plugin_dir = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow::anyhow!("--plugin-dir requires a path"))?,
                ));
            }
            "--slint-dir" => {
                slint_dir = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow::anyhow!("--slint-dir requires a path"))?,
                ));
            }
            "--app-theme-json" => {
                app_theme_json = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow::anyhow!("--app-theme-json requires a path"))?,
                ));
            }
            "--components" => {
                let value = args.next().ok_or_else(|| anyhow::anyhow!("--components requires a value"))?;
                components =
                    value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
            }
            "-h" | "--help" => {
                println!(
                    "usage: foundation-theme-compiler --json-dir <dir> --rust-dir <dir>\n       \
                     [--plugin-dir <dir> --slint-dir <dir> --app-theme-json <path> [--components a,b]]"
                );
                return Ok(Vec::new());
            }
            other => anyhow::bail!("unexpected argument: {other}"),
        }
    }

    let json_dir = json_dir.ok_or_else(|| anyhow::anyhow!("--json-dir is required"))?;
    let rust_dir = rust_dir.ok_or_else(|| anyhow::anyhow!("--rust-dir is required"))?;

    let ids = foundation_themes::build::compile_theme_dir(&json_dir, &rust_dir)?;

    // Per-app component theme `.slint` emission (button-first). Only runs when the
    // caller passes the slint trio; otherwise this is a pure Rust theme compile.
    if let (Some(plugin_dir), Some(slint_dir), Some(app_theme_json)) =
        (plugin_dir.as_ref(), slint_dir.as_ref(), app_theme_json.as_ref())
    {
        if components.is_empty() {
            components.push("button".to_string());
        }
        let keys: Vec<&str> = components.iter().map(String::as_str).collect();
        foundation_themes::build::compile_app_component_themes(app_theme_json, plugin_dir, slint_dir, &keys)?;
    }

    Ok(ids)
}
