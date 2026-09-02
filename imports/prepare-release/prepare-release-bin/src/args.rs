// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {clap::Parser, semver::Version, std::ffi::OsString, std::path::PathBuf};

/// Program arguments loaded from the CLI.
#[derive(Debug, Clone, Parser)]
#[command(name = "prepare-release")]
#[command(about = "Prepare a new KeyOS release")]
#[command(disable_version_flag = true)]
#[command(after_help = "\
This command performs the complete release process:
  1. Validates EXTRA_ENTROPY environment variable
  2. Builds all firmware (bootloader, recovery, main OS)
  3. Signs the bootloader using SAM-BA cipher
  4. Pushes to KeyOS-Releases-private and creates a PR

REQUIRED ENVIRONMENT VARIABLES:
  EXTRA_ENTROPY            64-character hex string for bootloader entropy
  SECURE_SAMBA_CIPHER_PATH Path to secure-sam-ba-cipher.py

OPTIONAL FLAGS:
  --log-serial        Pass through to `cargo xtask build-all`
  --log-usb-serial         Pass through to `cargo xtask build-all`
  --log-usb-file       Pass through to `cargo xtask build-all`

EXAMPLE:
  EXTRA_ENTROPY=... prepare-release 1.2.3 ~/secrets/SAM-BA")]
pub struct Args {
    /// Version to create (e.g., "1.2.3" or "v1.2.3").
    #[arg(value_parser = parse_version)]
    pub version: Version,

    /// Path to secrets directory containing sam-ba-license-activation.txt and cust.key.
    pub secrets_dir: PathBuf,

    /// Pass `--log-serial` through to `cargo xtask build-all`.
    #[arg(long)]
    pub log_serial: bool,

    /// Pass `--log-usb-serial` through to `cargo xtask build-all`.
    #[arg(long)]
    pub log_usb_serial: bool,

    /// Pass `--log-usb-file` through to `cargo xtask build-all`.
    #[arg(long)]
    pub log_usb_file: bool,
}

fn parse_version(input: &str) -> Result<Version, String> {
    let normalized = input.strip_prefix('v').unwrap_or(input);
    if normalized.matches('.').count() != 2 {
        return Err("KeyOS versions must contain exactly two periods for RecoveryOS compatibility".into());
    }
    let version = normalized.parse::<Version>().map_err(|error| format!("invalid version '{input}': {error}"))?;
    if version.to_string() != normalized {
        return Err("KeyOS versions must use canonical SemVer".into());
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn release_versions_are_recoveryos_compatible() {
        assert_eq!(parse_version("v1.4.0-beta3").unwrap().to_string(), "1.4.0-beta3");
        assert!(parse_version("1.4.0-beta.3").unwrap_err().contains("exactly two periods"));
    }
}

pub fn args<I, T>(args: I) -> Result<Args, Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    Args::try_parse_from(args).map_err(Error::Cli)
}

#[derive(Debug)]
pub enum Error {
    Cli(clap::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cli(e) => write!(f, "{}", e.render().ansi()),
        }
    }
}

impl std::error::Error for Error {}
