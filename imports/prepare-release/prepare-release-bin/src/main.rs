// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    colored::Colorize,
    std::{ffi::OsString, io::Write, process::ExitCode},
};

mod args;
mod build;
mod git;
mod sign;
mod validation;

fn main() -> ExitCode {
    let code = main_args(std::env::args_os(), &mut std::io::stdout(), &mut std::io::stderr());
    code.into()
}

fn main_args<I, T>(args: I, stdout: impl Write, mut stderr: impl Write) -> InternalExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    match run(args, stdout) {
        Ok(()) => InternalExitCode(0),
        Err(Error::Args(e @ args::Error::Cli(_))) => {
            // Clap already does the "error: {}" formatting.
            writeln!(stderr, "{e}").expect("write error to stderr");
            InternalExitCode(1)
        }
        Err(e) => {
            writeln!(stderr, "{} {e}", "error:".bold().red()).expect("write error to stderr");
            InternalExitCode(1)
        }
    }
}

fn run<I, T>(args: I, mut stdout: impl Write) -> Result<(), Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = args::args(args)?;

    writeln!(stdout, "{}", "═".repeat(50).bold()).map_err(Error::Stdout)?;
    writeln!(stdout, "Preparing KeyOS release {}", args.version.to_string().cyan().bold())
        .map_err(Error::Stdout)?;
    writeln!(stdout, "{}", "═".repeat(50).bold()).map_err(Error::Stdout)?;
    writeln!(stdout).map_err(Error::Stdout)?;

    // Step 1: Validate EXTRA_ENTROPY
    writeln!(stdout, "{} Validating EXTRA_ENTROPY...", "▶".blue()).map_err(Error::Stdout)?;
    validation::validate_extra_entropy()?;
    writeln!(stdout, "{} EXTRA_ENTROPY validated (64 hex chars)", "✓".green()).map_err(Error::Stdout)?;
    writeln!(stdout).map_err(Error::Stdout)?;

    // Step 2: Build firmware
    writeln!(stdout, "{} Building firmware...", "▶".blue()).map_err(Error::Stdout)?;
    build::build_firmware(
        args.log_serial,
        args.log_usb_serial,
        args.log_usb_file,
        &mut stdout,
    )?;
    build::verify_firmware_files_build()?;
    writeln!(stdout, "{} Firmware built successfully", "✓".green()).map_err(Error::Stdout)?;
    writeln!(stdout).map_err(Error::Stdout)?;

    // Step 3: Sign bootloader
    writeln!(stdout, "{} Signing bootloader...", "▶".blue()).map_err(Error::Stdout)?;
    sign::sign_bootloader(&args.secrets_dir, &mut stdout)?;
    writeln!(stdout, "{} Bootloader signed (boot.cip created)", "✓".green()).map_err(Error::Stdout)?;
    writeln!(stdout).map_err(Error::Stdout)?;

    // Step 4: Push to KeyOS-Releases-private
    writeln!(stdout, "{} Pushing to KeyOS-Releases-private...", "▶".blue()).map_err(Error::Stdout)?;
    let firmware_paths = build::verify_firmware_files_push()?;
    let summary = git::handle_release(&args, &firmware_paths, &mut stdout)?;

    // Print final summary
    print_summary(&args.version, &summary, &mut stdout)?;

    Ok(())
}

fn print_summary(
    version: &semver::Version,
    summary: &git::ReleaseSummary,
    stdout: &mut impl Write,
) -> Result<(), Error> {
    writeln!(stdout).map_err(Error::Stdout)?;
    writeln!(stdout, "{}", "═".repeat(50).bold()).map_err(Error::Stdout)?;
    writeln!(stdout, "{}", "Release Preparation Complete".bold()).map_err(Error::Stdout)?;
    writeln!(stdout, "{}", "═".repeat(50).bold()).map_err(Error::Stdout)?;
    writeln!(stdout).map_err(Error::Stdout)?;

    writeln!(stdout, "Version: {}", version.to_string().cyan()).map_err(Error::Stdout)?;
    writeln!(stdout).map_err(Error::Stdout)?;

    if summary.branch_created {
        writeln!(stdout, "{} Release branch created", "✓".green()).map_err(Error::Stdout)?;
    }

    if summary.files_copied {
        writeln!(stdout, "{} Files copied to release directory", "✓".green()).map_err(Error::Stdout)?;
    }

    if summary.changes_committed {
        writeln!(stdout, "{} Changes committed", "✓".green()).map_err(Error::Stdout)?;
    }

    if summary.branch_pushed {
        writeln!(stdout, "{} Branch pushed to GitHub", "✓".green()).map_err(Error::Stdout)?;
    }

    // PR status with appropriate symbol and message
    match &summary.pr_status {
        git::PrStatus::Created => {
            writeln!(stdout, "{} Pull Request created", "✓".green()).map_err(Error::Stdout)?;
        }
        git::PrStatus::AlreadyExists => {
            writeln!(stdout, "{} Pull Request already exists (kept existing)", "✓".yellow())
                .map_err(Error::Stdout)?;
        }
        git::PrStatus::GhNotAvailable => {
            writeln!(stdout, "{} Pull Request not created (gh CLI not available)", "!".yellow())
                .map_err(Error::Stdout)?;
        }
    }

    writeln!(stdout).map_err(Error::Stdout)?;
    writeln!(stdout, "{}", "═".repeat(50).bold()).map_err(Error::Stdout)?;
    writeln!(stdout, "{} Release {} preparation complete!", "✓".green().bold(), version)
        .map_err(Error::Stdout)?;
    writeln!(stdout, "{}", "═".repeat(50).bold()).map_err(Error::Stdout)?;

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InternalExitCode(u8);

impl From<InternalExitCode> for ExitCode {
    fn from(code: InternalExitCode) -> Self { code.0.into() }
}

#[derive(Debug)]
enum Error {
    Args(args::Error),
    Validation(validation::Error),
    Build(build::Error),
    Sign(sign::Error),
    Git(git::Error),
    Stdout(std::io::Error),
}

impl From<args::Error> for Error {
    fn from(e: args::Error) -> Self { Error::Args(e) }
}

impl From<validation::Error> for Error {
    fn from(e: validation::Error) -> Self { Error::Validation(e) }
}

impl From<build::Error> for Error {
    fn from(e: build::Error) -> Self { Error::Build(e) }
}

impl From<sign::Error> for Error {
    fn from(e: sign::Error) -> Self { Error::Sign(e) }
}

impl From<git::Error> for Error {
    fn from(e: git::Error) -> Self { Error::Git(e) }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Args(e) => write!(f, "{e}"),
            Error::Validation(e) => write!(f, "{e}"),
            Error::Build(e) => write!(f, "{e}"),
            Error::Sign(e) => write!(f, "{e}"),
            Error::Git(e) => write!(f, "{e}"),
            Error::Stdout(e) => write!(f, "failed to write to stdout: {e}"),
        }
    }
}

impl std::error::Error for Error {}
