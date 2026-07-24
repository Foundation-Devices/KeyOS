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
    // A `foundation-*` binary on PATH is exec'd before clap sees the arguments.
    commands::plugin::dispatch_external(&std::env::args().collect::<Vec<_>>());

    match Cli::parse().command {
        Commands::New(args) => commands::new::execute(&args)?,
        Commands::Develop => commands::develop::execute()?,
        Commands::Exit => commands::exit::execute()?,
        Commands::Build(args) => commands::build::execute(&args)?,
        Commands::Clean => commands::clean::execute()?,
        Commands::Sideload(args) => commands::sideload::execute(&args)?,
        Commands::Sim => commands::sim::execute()?,
        Commands::Cert(args) => commands::cert::execute(&args)?,
        Commands::Theme(args) => commands::theme::execute(&args)?,
        Commands::Themes(args) => commands::themes::execute(&args)?,
        Commands::Doctor => commands::doctor::execute()?,
        Commands::Preview(args) => commands::preview::execute(&args)?,
        Commands::Logs(args) => commands::logs::execute(&args)?,
        Commands::Plugin(args) => commands::plugin::execute(&args).await?,
        Commands::Completions(args) => commands::completions::execute(&args)?,
    }

    Ok(())
}
