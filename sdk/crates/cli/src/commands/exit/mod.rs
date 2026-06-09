// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Cleanup command for the Foundation development environment.

use std::fs;
use std::process::Command;

use anyhow::{Context, Result};
use foundation_ui::TerminalUI;

/// Execute the `foundation exit` cleanup flow.
pub fn execute() -> Result<()> {
    let ui = TerminalUI::new();

    println!("Cleaning up Foundation development environment...");
    println!();

    // Run Nix garbage collection
    let spinner = ui.spinner("Running Nix garbage collection...");
    match run_garbage_collection() {
        Ok(()) => {
            spinner.finish_success("Garbage collection complete");
        }
        Err(_) => {
            spinner.finish_clear();
            ui.info("Garbage collection failed (this is normal if you haven't used 'develop' yet)");
        }
    }

    // Remove Nix cache
    let spinner = ui.spinner("Removing Nix cache...");
    match remove_nix_cache() {
        Ok(removed) => {
            if removed {
                spinner.finish_success("Nix cache removed successfully");
            } else {
                spinner.finish_clear();
                ui.info("No Nix cache found to remove");
            }
        }
        Err(e) => {
            spinner.finish_error(&format!("Failed to remove Nix cache: {}", e));
        }
    }

    println!();
    println!();
    println!("Cleanup complete! Installed SDK bundles and signing identities were left untouched.");

    Ok(())
}

/// Run Nix garbage collection
fn run_garbage_collection() -> Result<()> {
    let status = Command::new("nix-collect-garbage")
        .arg("-d")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("Failed to run nix-collect-garbage")?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("nix-collect-garbage exited with error")
    }
}

/// Remove Nix cache directory
fn remove_nix_cache() -> Result<bool> {
    let home_dir = dirs::home_dir().context("Could not determine home directory")?;

    let cache_dir = home_dir.join(".cache").join("nix");

    if cache_dir.exists() {
        fs::remove_dir_all(&cache_dir)
            .with_context(|| format!("Failed to remove cache directory: {}", cache_dir.display()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}
