// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::PathBuf;

#[path = "../plugin/mod.rs"]
mod plugin;
#[path = "../theme_export.rs"]
mod theme_export;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(flag) = args.next() else {
        return Err("usage: cargo run --bin themegen -- --output-dir <path>".into());
    };
    if flag != "--output-dir" {
        return Err(format!("unsupported argument: {flag}").into());
    }
    let Some(output_dir) = args.next() else {
        return Err("missing output dir".into());
    };
    if args.next().is_some() {
        return Err("unexpected extra arguments".into());
    }

    theme_export::write_theme_crate_outputs(&PathBuf::from(output_dir))
}
