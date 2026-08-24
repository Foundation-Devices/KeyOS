// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Pack command - build, sign, and wrap the app bundle into a single installable archive.

use std::path::PathBuf;

use anyhow::{Context, Result};
use app_archive::{archive_file_name, pack_bundle};
use clap::Args;
use foundation_core::ProjectContext;

use crate::commands::build::{self, BuildArgs};
use crate::commands::format_bytes;

#[derive(Args)]
pub struct PackArgs {
    /// Build in release mode with optimizations before packing
    #[arg(short, long)]
    pub release: bool,

    /// Write the archive here instead of next to the built bundle
    #[arg(short, long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

pub fn execute(args: &PackArgs) -> Result<()> {
    println!("Building and packing the application...");
    let built = build::execute(&BuildArgs { release: args.release })?;

    let project = ProjectContext::discover()?;
    let config = &project.config;
    let archive_path = match &args.out {
        Some(out) => out.clone(),
        None => built.bundle_dir.with_file_name(archive_file_name(&config.app_name)),
    };

    let report = pack_bundle(&built.bundle_dir, &archive_path, &built.hashed_files)
        .with_context(|| format!("Failed to pack {}", built.bundle_dir.display()))?;

    println!();
    println!("Pack complete!");
    println!("Output: {}", report.archive_path.display());
    println!("  {} files, {}", report.entries, format_bytes(report.archive_bytes));
    println!("Version: {}", built.version);
    println!();
    println!("Copy the archive to a USB drive or the airlock, then install it on Passport Prime");
    println!("from Settings > Apps > Install App.");

    Ok(())
}
