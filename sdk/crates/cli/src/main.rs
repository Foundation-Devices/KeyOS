// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Foundation CLI entry point

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};

mod assets;
mod cargo_support;
mod cli;
mod commands;
mod sdk_mapping;
mod signing_permissions;
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
    // External command dispatch is quarantined with the rest of the plugin
    // surface until the plugin trust model is ready for supported releases.
    #[cfg(feature = "experimental-plugins")]
    commands::plugin::dispatch_external(&std::env::args().collect::<Vec<_>>());

    match Cli::parse().command {
        Commands::New(args) => commands::new::execute(&args)?,
        Commands::Develop => commands::develop::execute()?,
        Commands::Exit => commands::exit::execute()?,
        Commands::Build(args) => {
            commands::build::execute(&args)?;
        }
        Commands::Clean => commands::clean::execute()?,
        Commands::Pack(args) => commands::pack::execute(&args)?,
        Commands::Sideload(args) => commands::sideload::execute(&args)?,
        Commands::Sim => commands::sim::execute()?,
        Commands::Cert(args) => commands::cert::execute(&args)?,
        Commands::Theme(args) => commands::theme::execute(&args)?,
        Commands::Themes(args) => commands::themes::execute(&args)?,
        Commands::Doctor => commands::doctor::execute()?,
        Commands::Docs(args) => commands::docs::execute(&args)?,
        Commands::Preview(args) => commands::preview::execute(&args)?,
        Commands::Logs(args) => commands::logs::execute(&args)?,
        #[cfg(feature = "experimental-plugins")]
        Commands::Plugin(args) => commands::plugin::execute(&args).await?,
        Commands::Completions(args) => commands::completions::execute(&args)?,
    }

    Ok(())
}
