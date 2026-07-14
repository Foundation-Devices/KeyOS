// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Logs command - launch the Passport USB log viewer

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use clap::Args;
use foundation_core::SdkRoot;

const LOG_VIEWER_NAMES: &[&str] = &["foundation-keyos-log-viewer", "keyos-log-viewer"];

#[derive(Args)]
pub struct LogsArgs {
    /// Reconnect timeout in seconds before retrying USB discovery (default: 3)
    #[arg(short, long, value_name = "SECONDS", default_value_t = 3)]
    pub timeout: u64,
}

pub fn execute(args: &LogsArgs) -> Result<()> {
    let sdk = SdkRoot::discover().ok();
    let viewer = sdk
        .as_ref()
        .and_then(|sdk| sdk.tool_path(LOG_VIEWER_NAMES))
        .or_else(|| find_in_path(LOG_VIEWER_NAMES))
        .ok_or_else(|| anyhow::anyhow!("foundation-keyos-log-viewer not found. Make sure you're in the Foundation development environment or using an installed SDK bundle."))?;

    let mut cmd = Command::new(viewer);
    cmd.arg("--timeout").arg(args.timeout.to_string());

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
