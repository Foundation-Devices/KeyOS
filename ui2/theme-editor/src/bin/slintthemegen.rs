// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! slintthemegen — writes the shared default `<key>_theme.slint` files into
//! `ui2/components/ui` from the plugin JSON schema (`defaults/plugins/*.json`).
//!
//! The emitter itself lives in `components::theme_gen` (shared with the
//! `foundation build` per-app generator). This binary just loads every plugin
//! and emits the *no-overrides* default beside `theme.slint`; per-app overrides
//! are applied by the theme-compile step, not here.
//!
//!   cargo run --bin slintthemegen -- --component checkbox   # print one to stdout
//!   cargo run --bin slintthemegen -- --out-dir <dir>        # write every file
//!   cargo run --bin slintthemegen -- --check                # CI staleness guard

// This helper only needs the repo-default plugin loader, but it shares the
// theme editor's runtime plugin module so both paths normalize schemas the same
// way.
#[allow(dead_code, unused_imports)]
#[path = "../plugin/mod.rs"]
mod plugin;

use std::path::PathBuf;

use components::theme_gen::component_theme_slint;
use plugin::load_all_plugins_from_repo;

/// Shared default files sit beside `theme.slint`, so a relative import resolves.
/// (The per-app generator uses `@ui/theme.slint` instead.)
const THEME_IMPORT: &str = "theme.slint";

fn main() {
    let mut component: Option<String> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--component" => component = args.next(),
            "--out-dir" => out_dir = args.next().map(PathBuf::from),
            "--check" => check = true,
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }

    let plugins = match load_all_plugins_from_repo() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("failed to load plugins: {e}");
            std::process::exit(1);
        }
    };

    if let Some(key) = component {
        for (spec, plugin) in &plugins {
            if spec.key == key {
                match component_theme_slint(&spec.key, plugin, None, THEME_IMPORT) {
                    Some(text) => print!("{text}"),
                    None => eprintln!("{key}: no sizeProps/variantProps (nothing to emit)"),
                }
                return;
            }
        }
        eprintln!("component not found: {key}");
        std::process::exit(1);
    }

    let out_dir = out_dir.unwrap_or_else(|| PathBuf::from("ui2/components/ui"));

    // --check: verify the committed *_theme.slint match what the generator would
    // emit from the current plugin schemas. CI guard against stale theme files.
    if check {
        let mut stale = Vec::new();
        for (spec, plugin) in &plugins {
            if let Some(text) = component_theme_slint(&spec.key, plugin, None, THEME_IMPORT) {
                let path = out_dir.join(format!("{}_theme.slint", spec.key));
                if std::fs::read_to_string(&path).unwrap_or_default() != text {
                    stale.push(spec.key.to_string());
                }
            }
        }
        if stale.is_empty() {
            eprintln!("component theme files are up to date with the plugin schemas");
        } else {
            eprintln!("STALE component theme files (run `just gen-themes`): {}", stale.join(", "));
            std::process::exit(1);
        }
        return;
    }

    let mut written = 0;
    for (spec, plugin) in &plugins {
        if let Some(text) = component_theme_slint(&spec.key, plugin, None, THEME_IMPORT) {
            let path = out_dir.join(format!("{}_theme.slint", spec.key));
            if let Err(e) = std::fs::write(&path, text) {
                eprintln!("write {}: {e}", path.display());
                std::process::exit(1);
            }
            println!("wrote {}", path.display());
            written += 1;
        }
    }
    eprintln!("{written} component theme file(s) written to {}", out_dir.display());
}
