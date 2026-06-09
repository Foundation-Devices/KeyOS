// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use anyhow::Result;
use clap::{Args, Subcommand};

pub(crate) mod flux;

#[derive(Args)]
pub struct TestArgs {
    #[command(subcommand)]
    command: TestCommand,
}

#[derive(Subcommand)]
enum TestCommand {
    /// Run Flux Ledger-HID APDU smoke tests through passport-drive
    Flux(flux::Args),
}

pub fn run(args: TestArgs) -> Result<()> {
    match args.command {
        TestCommand::Flux(args) => flux::run(args),
    }
}
