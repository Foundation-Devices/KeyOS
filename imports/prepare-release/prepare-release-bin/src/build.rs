// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use semver::Version;
use std::{env, fs, io::Write, path::PathBuf, process::Command};

const FOUNDATION_SIDELOAD_APP_COUNT: usize = 3;

/// Paths to the firmware files
#[derive(Debug, Clone)]
pub struct FirmwarePaths {
    pub bootloader_cip: PathBuf,
    pub recovery: PathBuf,
    pub app: PathBuf,
    pub apps_dir: Option<PathBuf>,
    pub sideload_apps_dir: PathBuf,
    pub blassets_dir: PathBuf,
    pub common_assets_boot_dir: Option<PathBuf>,
    pub common_assets_dir: Option<PathBuf>,
}

/// Build firmware components (bootloader + recovery + main OS)
pub fn build_firmware(
    version: &Version,
    log_serial: bool,
    log_usb_serial: bool,
    log_usb_file: bool,
    stdout: &mut impl Write,
) -> Result<(), Error> {
    let extra_entropy = env::var("EXTRA_ENTROPY").map_err(|_| Error::ExtraEntropyMissing)?;

    writeln!(stdout, "Building firmware (bootloader + recovery + main OS)...").map_err(Error::Stdout)?;

    let mut args = production_build_args(version, extra_entropy);

    if log_serial {
        writeln!(stdout, "  - Enabling UART serial logging").map_err(Error::Stdout)?;
        args.push("--log-serial".to_string());
    }

    if log_usb_serial {
        writeln!(stdout, "  - Enabling USB serial logging").map_err(Error::Stdout)?;
        args.push("--log-usb-serial".to_string());
    }

    if log_usb_file {
        writeln!(stdout, "  - Enabling external USB file log logging").map_err(Error::Stdout)?;
        args.push("--log-usb-file".to_string());
    }

    let build_result = Command::new("cargo")
        .args(&args)
        .envs(env::vars()) // Inherit all environment variables including PATH
        .status()
        .map_err(Error::CommandFailed)?;

    if !build_result.success() {
        return Err(Error::BootloaderBuildFailed);
    }

    Ok(())
}

fn production_build_args(version: &Version, extra_entropy: String) -> Vec<String> {
    vec![
        "xtask".to_string(),
        "build-all".to_string(),
        "--dont-sign".to_string(), /* --dont-sign because the files will be signed by a `signer` tool in
                                    * KeyOS-Releases-private */
        "--production-bootloader".to_string(),
        "--production-firmware".to_string(),
        "--keyos-version".to_string(),
        version.to_string(),
        "--extra-entropy".to_string(),
        extra_entropy,
    ]
}

/// Verify that firmware files were built successfully (build phase).
/// This checks for boot.bin (the unsigned bootloader) which will be signed later.
pub fn verify_firmware_files_build() -> Result<(), Error> {
    let bootloader_bin = PathBuf::from("target/armv7a-unknown-xous-elf/release/images/boot.bin");
    let recovery_bin = PathBuf::from("target/armv7a-unknown-xous-elf/release/images/recovery.bin");
    let app_bin = PathBuf::from("target/armv7a-unknown-xous-elf/release/images/app.bin");
    let blassets_dir = PathBuf::from("boot/keyos-boot/assets");

    // Verify bootloader (boot.bin) exists - this will be signed to create boot.cip
    if !bootloader_bin.exists() {
        return Err(Error::FirmwareNotFound {
            firmware: "bootloader (boot.bin)".to_string(),
            path: bootloader_bin,
        });
    }

    // Verify recovery exists
    if !recovery_bin.exists() {
        return Err(Error::FirmwareNotFound {
            firmware: "recovery firmware".to_string(),
            path: recovery_bin,
        });
    }

    // Verify app exists
    if !app_bin.exists() {
        return Err(Error::FirmwareNotFound { firmware: "main firmware".to_string(), path: app_bin });
    }

    // Validate blassets directory exists and contains .raw files
    if !blassets_dir.exists() {
        return Err(Error::BlassetsNotFound { path: blassets_dir });
    }

    // Check that there's at least one .raw file
    let has_raw_files = fs::read_dir(&blassets_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|entry| entry.path().extension().is_some_and(|ext| ext == "raw"))
        })
        .unwrap_or(false);

    if !has_raw_files {
        return Err(Error::BlassetsEmpty { path: blassets_dir });
    }

    Ok(())
}

/// Verify that firmware files exist for push phase.
/// This checks for boot.cip (the signed bootloader) - boot.bin must NOT be uploaded.
pub fn verify_firmware_files_push() -> Result<FirmwarePaths, Error> {
    let bootloader_cip = PathBuf::from("target/armv7a-unknown-xous-elf/release/images/boot.cip");
    let recovery_bin = PathBuf::from("target/armv7a-unknown-xous-elf/release/images/recovery.bin");
    let app_bin = PathBuf::from("target/armv7a-unknown-xous-elf/release/images/app.bin");
    let apps_dir = PathBuf::from("target/armv7a-unknown-xous-elf/release/keyos/apps");
    let sideload_apps_dir =
        PathBuf::from("target/armv7a-unknown-xous-elf/release/sideload-bundles-unsigned");
    let blassets_dir = PathBuf::from("boot/keyos-boot/assets");
    let common_assets_boot_dir = PathBuf::from("target/armv7a-unknown-xous-elf/release/common-boot");
    let common_assets_dir = PathBuf::from("target/armv7a-unknown-xous-elf/release/common");

    // Verify signed bootloader (boot.cip) exists
    // boot.cip is created by `just prepare-release` which signs the bootloader with SAM-BA cipher
    // boot.bin is NOT uploaded to the repository because it contains unencrypted EXTRA_ENTROPY
    if !bootloader_cip.exists() {
        return Err(Error::BootCipNotFound { path: bootloader_cip });
    }

    // Verify recovery exists
    if !recovery_bin.exists() {
        return Err(Error::FirmwareNotFound {
            firmware: "recovery firmware".to_string(),
            path: recovery_bin,
        });
    }

    // Verify app exists
    if !app_bin.exists() {
        return Err(Error::FirmwareNotFound { firmware: "main firmware".to_string(), path: app_bin });
    }

    let apps_dir_exists = apps_dir.exists();
    let sideload_apps_valid = fs::read_dir(&sideload_apps_dir)
        .map(|entries| {
            let bundles: Vec<_> = entries.filter_map(Result::ok).filter(|entry| entry.path().is_dir()).collect();
            bundles.len() == FOUNDATION_SIDELOAD_APP_COUNT
                && bundles.iter().all(|entry| {
                    entry.path().join("app.elf").is_file()
                        && entry.path().join("manifest.json").is_file()
                })
        })
        .unwrap_or(false);
    if !sideload_apps_valid {
        return Err(Error::FirmwareNotFound {
            firmware: "Foundation sideload app bundles".to_string(),
            path: sideload_apps_dir,
        });
    }
    let common_assets_boot_dir_exists = common_assets_boot_dir.exists();
    let common_assets_dir_exists = common_assets_dir.exists();

    // Validate blassets directory exists and contains .raw files
    if !blassets_dir.exists() {
        return Err(Error::BlassetsNotFound { path: blassets_dir });
    }

    // Check that there's at least one .raw file
    let has_raw_files = fs::read_dir(&blassets_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|entry| entry.path().extension().is_some_and(|ext| ext == "raw"))
        })
        .unwrap_or(false);

    if !has_raw_files {
        return Err(Error::BlassetsEmpty { path: blassets_dir });
    }

    Ok(FirmwarePaths {
        bootloader_cip,
        recovery: recovery_bin,
        app: app_bin,
        apps_dir: if apps_dir_exists { Some(apps_dir) } else { None },
        sideload_apps_dir,
        blassets_dir,
        common_assets_dir: if common_assets_dir_exists { Some(common_assets_dir) } else { None },
        common_assets_boot_dir: if common_assets_boot_dir_exists {
            Some(common_assets_boot_dir)
        } else {
            None
        },
    })
}

#[derive(Debug)]
pub enum Error {
    ExtraEntropyMissing,
    CommandFailed(std::io::Error),
    BootloaderBuildFailed,
    FirmwareNotFound { firmware: String, path: PathBuf },
    BootCipNotFound { path: PathBuf },
    BlassetsNotFound { path: PathBuf },
    BlassetsEmpty { path: PathBuf },
    Stdout(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ExtraEntropyMissing => {
                write!(f, "EXTRA_ENTROPY environment variable is not set")
            }

            Error::CommandFailed(e) => {
                write!(f, "failed to execute build command: {}", e)
            }
            Error::BootloaderBuildFailed => {
                write!(f, "failed to build bootloader\n\
                          This likely means the ARM cross-compiler toolchain (arm-none-eabi-gcc) is not installed.\n\
                          Please install the ARM toolchain or check the build requirements.")
            }
            Error::FirmwareNotFound { firmware, path } => {
                write!(f, "{} not found at {}", firmware, path.display())
            }
            Error::BootCipNotFound { path } => {
                write!(f, "signed bootloader (boot.cip) not found at {}", path.display())
            }
            Error::BlassetsNotFound { path } => {
                write!(
                    f,
                    "bootloader assets directory not found at {}\n\
                          This directory is required for factory builds.\n\
                          Make sure the bootloader was built successfully.",
                    path.display()
                )
            }
            Error::BlassetsEmpty { path } => {
                write!(
                    f,
                    "no .raw files found in bootloader assets directory at {}\n\
                          Factory builds require bootloader splash screen assets.\n\
                          Make sure the bootloader was built successfully.",
                    path.display()
                )
            }
            Error::Stdout(e) => {
                write!(f, "failed to write to stdout: {}", e)
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::production_build_args;
    use semver::Version;

    #[test]
    fn production_build_receives_the_release_version() {
        let args = production_build_args(&Version::parse("1.4.0-beta3").unwrap(), "entropy".into());
        assert!(args.windows(2).any(|pair| pair == ["--keyos-version", "1.4.0-beta3"]));
    }
}
