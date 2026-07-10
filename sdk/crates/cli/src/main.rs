// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Foundation CLI entry point

use anyhow::{Context, Result};
use foundation_plugins::{exec_plugin, CommandRegistry, ResolvedCommand};

mod assets;
mod cargo_support;
mod cli;
mod commands;
mod sdk_mapping;
mod slint_codegen;
#[cfg(test)]
mod test_support;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if matches!(args.get(1).map(String::as_str), Some("-V" | "--version")) && args.len() == 2 {
        println!("foundation {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if let Some(command_name) = args.get(1) {
        let builtins = cli::builtin_top_level_names();

        let is_global = matches!(command_name.as_str(), "-h" | "--help" | "-V" | "--version");
        let is_builtin = builtins.iter().any(|builtin| builtin == command_name);

        if !is_global && !is_builtin {
            let mut registry = CommandRegistry::new();
            if let Some(ResolvedCommand::External(path)) = registry.resolve(command_name) {
                exec_plugin(&path, &args[2..]);
            }
        }
    }

    // Build and parse arguments
    let matches = cli::build_cli().get_matches_from(args);

    // Handle commands
    match matches.subcommand() {
        Some((subcommand_name, sub_matches)) if subcommand_name == cli::CMD_NEW => {
            let app_name = sub_matches.get_one::<String>("name").map(|s| s.as_str());
            let template = sub_matches.get_one::<String>("template").map(|s| s.as_str());
            let no_git = sub_matches.get_flag("no-git");

            // Execute the new command
            commands::new::execute(app_name, template, no_git)?;
        }
        Some((subcommand_name, _sub_matches)) if subcommand_name == cli::CMD_DEVELOP => {
            // Execute the develop command
            commands::develop::execute()?;
        }
        Some((subcommand_name, _sub_matches)) if subcommand_name == cli::CMD_EXIT => {
            // Execute the exit command
            commands::exit::execute()?;
        }
        Some((subcommand_name, sub_matches)) if subcommand_name == cli::CMD_BUILD => {
            let release = sub_matches.get_flag("release");
            commands::build::execute(release)?;
        }
        Some((subcommand_name, _sub_matches)) if subcommand_name == cli::CMD_CLEAN => {
            commands::clean::execute()?;
        }
        Some((subcommand_name, sub_matches)) if subcommand_name == cli::CMD_SIDELOAD => {
            let release = sub_matches.get_flag("release");
            let no_run = sub_matches.get_flag("no-run");
            commands::sideload::execute(release, no_run)?;
        }
        Some((subcommand_name, _sub_matches)) if subcommand_name == cli::CMD_SIM => {
            commands::sim::execute()?;
        }
        Some((subcommand_name, sub_matches)) if subcommand_name == cli::CMD_CERT => {
            match sub_matches.subcommand() {
                Some((cert_command_name, sub_matches)) if cert_command_name == cli::CMD_CERT_GEN => {
                    let key_name = sub_matches.get_one::<String>("name").map(|s| s.as_str());
                    let publisher_name = sub_matches.get_one::<String>("publisher-name").map(|s| s.as_str());
                    let contact_email = sub_matches.get_one::<String>("contact-email").map(|s| s.as_str());
                    let support_url = sub_matches.get_one::<String>("support-url").map(|s| s.as_str());
                    commands::cert::execute_gen(key_name, publisher_name, contact_email, support_url)?;
                }
                Some((cert_command_name, sub_matches)) if cert_command_name == cli::CMD_CERT_PRINT => {
                    let key_name = sub_matches.get_one::<String>("name").map(|s| s.as_str());
                    commands::cert::execute_print(key_name)?;
                }
                Some((cert_command_name, sub_matches)) if cert_command_name == cli::CMD_CERT_INSTALL => {
                    let key_name = sub_matches.get_one::<String>("name").map(|s| s.as_str());
                    commands::cert::execute_install(key_name)?;
                }
                _ => cli::print_help(),
            }
        }
        Some((subcommand_name, _sub_matches)) if subcommand_name == cli::CMD_THEME => {
            commands::theme::execute()?;
        }
        Some((subcommand_name, sub_matches)) if subcommand_name == cli::CMD_THEMES => {
            match sub_matches.subcommand() {
                Some((name, _)) if name == cli::CMD_THEMES_BUILD => {
                    commands::themes::execute_build()?;
                }
                Some((name, _)) if name == cli::CMD_THEMES_LIST => {
                    commands::themes::execute_list()?;
                }
                Some((name, sub_matches)) if name == cli::CMD_THEMES_NEW => {
                    let theme_name = sub_matches
                        .get_one::<String>("name")
                        .context("internal: required arg `name` missing")?;
                    let base =
                        sub_matches.get_one::<String>("from").map(String::as_str).unwrap_or("default_theme");
                    commands::themes::execute_new(theme_name, base)?;
                }
                _ => cli::print_help(),
            }
        }
        Some((subcommand_name, _sub_matches)) if subcommand_name == cli::CMD_DOCTOR => {
            commands::doctor::execute()?;
        }
        Some((subcommand_name, sub_matches)) if subcommand_name == cli::CMD_PREVIEW => {
            commands::preview::execute(sub_matches)?;
        }
        Some((subcommand_name, sub_matches)) if subcommand_name == cli::CMD_LOGS => {
            commands::logs::execute(sub_matches)?;
        }
        Some((subcommand_name, sub_matches)) if subcommand_name == cli::CMD_PLUGIN => {
            match sub_matches.subcommand() {
                Some((plugin_command_name, sub_matches))
                    if plugin_command_name == cli::CMD_PLUGIN_INSTALL =>
                {
                    let plugin = sub_matches
                        .get_one::<String>("plugin")
                        .context("internal: required arg `plugin` missing")?;
                    commands::install::execute(plugin).await?;
                }
                Some((plugin_command_name, sub_matches))
                    if plugin_command_name == cli::CMD_PLUGIN_UNINSTALL =>
                {
                    let plugin = sub_matches
                        .get_one::<String>("plugin")
                        .context("internal: required arg `plugin` missing")?;
                    commands::uninstall::execute(plugin)?;
                }
                Some((plugin_command_name, sub_matches)) if plugin_command_name == cli::CMD_PLUGIN_SEARCH => {
                    let query = sub_matches
                        .get_one::<String>("query")
                        .context("internal: required arg `query` missing")?;
                    commands::search::execute(query).await?;
                }
                _ => cli::print_help(),
            }
        }
        Some((subcommand_name, sub_matches)) if subcommand_name == cli::CMD_COMPLETIONS => {
            let shell =
                sub_matches.get_one::<String>("shell").context("internal: required arg `shell` missing")?;
            let install = sub_matches.get_flag("install");

            commands::completions::execute(shell, install)?;
        }
        _ => {
            // No subcommand - show help
            cli::print_help();
        }
    }

    Ok(())
}
