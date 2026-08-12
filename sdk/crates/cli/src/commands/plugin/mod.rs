// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `foundation plugin` - search, install, and uninstall CLI plugins.

use anyhow::Result;
use clap::{Args, CommandFactory, Subcommand};
use foundation_plugins::{exec_plugin, CommandRegistry, ResolvedCommand};

use crate::cli::Cli;
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

/// Git-style external dispatch for unknown, non-option command names.
///
/// Built-in commands and options always remain under clap's control, so an
/// external binary cannot shadow supported behavior or global flags.
pub fn dispatch_external(args: &[String]) {
    let Some(command_name) = args.get(1) else {
        return;
    };

    if !is_external_command_candidate(command_name) {
        return;
    }

    let mut registry = CommandRegistry::new();
    if let Some(ResolvedCommand::External(path)) = registry.resolve(command_name) {
        exec_plugin(&path, &args[2..]);
    }
}

fn is_external_command_candidate(command_name: &str) -> bool {
    if command_name.starts_with('-') {
        return false;
    }

    let command = Cli::command();
    let is_builtin = command.get_subcommands().any(|subcommand| {
        subcommand.get_name() == command_name
            || subcommand.get_all_aliases().any(|alias| alias == command_name)
    });
    !is_builtin
}

#[cfg(test)]
mod tests {
    use super::is_external_command_candidate;

    #[test]
    fn external_plugins_cannot_shadow_builtins_or_options() {
        assert!(!is_external_command_candidate("build"));
        assert!(!is_external_command_candidate("plugin"));
        assert!(!is_external_command_candidate("--help"));
        assert!(!is_external_command_candidate("-V"));
        assert!(is_external_command_candidate("third-party-command"));
    }
}
