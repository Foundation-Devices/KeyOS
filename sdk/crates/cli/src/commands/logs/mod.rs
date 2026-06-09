// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Logs command - launch the Passport USB log viewer

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use clap::ArgMatches;
use foundation_core::SdkRoot;

const LOG_VIEWER_NAMES: &[&str] = &["foundation-keyos-log-viewer", "keyos-log-viewer"];

pub fn execute(matches: &ArgMatches) -> Result<()> {
    let sdk = SdkRoot::discover().ok();
    let viewer = sdk
        .as_ref()
        .and_then(|sdk| sdk.tool_path(LOG_VIEWER_NAMES))
        .or_else(|| find_in_path(LOG_VIEWER_NAMES))
        .ok_or_else(|| anyhow::anyhow!("foundation-keyos-log-viewer not found. Make sure you're in the Foundation development environment or using an installed SDK bundle."))?;

    let mut cmd = Command::new(viewer);
    if let Some(timeout) = matches.get_one::<u64>("timeout") {
        cmd.arg("--timeout").arg(timeout.to_string());
    }

    let status = cmd.status().context("Failed to start foundation-keyos-log-viewer")?;
    if !status.success() {
        anyhow::bail!("Failed to start foundation-keyos-log-viewer");
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
