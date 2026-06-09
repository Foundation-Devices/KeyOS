// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Search for Foundation plugins

use anyhow::{Context, Result};
use foundation_plugins::install::{InstallError, PluginInstaller};

/// Execute the search command
pub async fn execute(query: &str) -> Result<()> {
    println!("Searching for '{}'...", query);
    println!();

    let installer = PluginInstaller::new();

    match installer.search(query).await {
        Ok(results) => {
            if results.is_empty() {
                println!("No plugins found matching '{}'", query);
            } else {
                println!("Found {} plugin(s):", results.len());
                println!();

                for plugin in &results {
                    // Plugin name with verified badge
                    if plugin.verified {
                        println!("  \x1b[1m{}\x1b[0m \x1b[32m[{}]\x1b[0m", plugin.name, "Verified");
                    } else {
                        println!("  \x1b[1m{}\x1b[0m", plugin.name);
                    }

                    // Description
                    println!("    {}", plugin.description);

                    // Repository
                    println!("    \x1b[2m{}\x1b[0m", plugin.repository);

                    // Tags
                    if !plugin.tags.is_empty() {
                        let tags_str = plugin.tags.join(", ");
                        println!("    \x1b[36m[{}]\x1b[0m", tags_str);
                    }

                    println!();
                }

                println!("Run 'foundation plugin install <name>' to install a plugin.");
            }

            Ok(())
        }
        Err(e) => {
            println!("  \x1b[31m✗\x1b[0m {}", format_error(&e));
            Err(e).context("Failed to search plugins")
        }
    }
}

fn format_error(e: &InstallError) -> String {
    match e {
        InstallError::IndexReadError(_, msg) => format!("Failed to read plugin index: {}", msg),
        InstallError::IndexParseError(_, msg) => format!("Failed to parse plugin index: {}", msg),
        _ => e.to_string(),
    }
}
