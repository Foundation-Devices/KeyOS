// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `foundation plugin` - search, install, and uninstall CLI plugins.

use anyhow::Result;
use clap::{Args, Subcommand};
use foundation_plugins::{exec_plugin, CommandRegistry, ResolvedCommand};

use crate::commands::{install, search, uninstall};

#[derive(Args)]
pub struct PluginArgs {
    #[command(subcommand)]
    pub command: PluginCommands,
}

#[derive(Subcommand)]
pub enum PluginCommands {
    /// Install a Foundation plugin
    #[command(long_about = "Downloads and installs a Foundation CLI plugin from the registry or GitHub")]
    Install(PluginInstallArgs),

    /// Uninstall a Foundation plugin
    #[command(long_about = "Removes an installed Foundation CLI plugin")]
    Uninstall(PluginUninstallArgs),

    /// Search for Foundation plugins
    #[command(long_about = "Searches the plugin registry for available plugins")]
    Search(PluginSearchArgs),
}

#[derive(Args)]
pub struct PluginInstallArgs {
    /// Plugin name or repository (e.g., 'foo' or 'user/repo')
    pub plugin: String,
}

#[derive(Args)]
pub struct PluginUninstallArgs {
    /// Plugin name to uninstall
    pub plugin: String,
}

#[derive(Args)]
pub struct PluginSearchArgs {
    /// Search query
    pub query: String,
}

pub async fn execute(args: &PluginArgs) -> Result<()> {
    match &args.command {
        PluginCommands::Install(args) => install::execute(&args.plugin).await,
        PluginCommands::Uninstall(args) => uninstall::execute(&args.plugin),
        PluginCommands::Search(args) => search::execute(&args.query).await,
    }
}

/// Git-style external dispatch: if the first argument resolves to a
/// `foundation-*` binary on PATH, replace this process with it. Runs before
/// clap, so an installed plugin may shadow a built-in command or global flag.
/// Returns when nothing matches, leaving the arguments for clap.
pub fn dispatch_external(args: &[String]) {
    let Some(command_name) = args.get(1) else {
        return;
    };

    let mut registry = CommandRegistry::new();
    if let Some(ResolvedCommand::External(path)) = registry.resolve(command_name) {
        exec_plugin(&path, &args[2..]);
    }
}
