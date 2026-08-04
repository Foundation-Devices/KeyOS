// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;

use bootimage::print_hashes;
use bootloader::{build_keyos_boot, BootloaderBuildArgs, SambaCryptArgs};
use clap::{Args, Parser, Subcommand};

use crate::bootimage::{build_charge_boot, create_boot_image};
use crate::builder::*;
use crate::flash::{dump_flash, flash_firmware, DumpFlashArgs, FlashArgs};
use crate::symbolicate::SymbolicateArgs;

mod bootimage;
mod bootloader;
mod builder;
mod docs_api;
mod elf;
mod flash;
mod hot_reload;
mod release_generator;
mod symbolicate;
mod system_disk;
mod tags;
mod tests;
mod utils;
mod xous_arguments;

const KEYOS_VERSION: &str = "1.3.0";

const BOOTLOADER_IMAGE: &str = "boot.bin";
const BOOTLOADER_IMAGE_CIPHER: &str = "boot_sama5d2x.cip";
const BOOT_ASSETS_DIR: &str = "blassets";

const APP_IMAGE: &str = "app.bin";
const RECOVERY_IMAGE: &str = "recovery.bin";

/// Logging output services.
const LOGGING_SERVICE_SERIAL: &str = "log-serial";
const LOGGING_SERVICE_USB_DEBUG: &str = "usb-debug";
const LOGGING_SERVICE_FILE: &str = "log-file";
const LOGGING_SERVICE_USB_FILE: &str = "log-usb-file";

const MANDATORY_SYSTEM_SERVICES_HW: &[&str] = &["xous-log", "xous-ticktimer", "xous-names", "trng"];

pub const MANDATORY_SYSTEM_SERVICES_HOSTED: &[&str] =
    &["xous-log", "log-hosted", "xous-ticktimer", "xous-names", "trng"];

const DEFAULT_SERVICES_RECOVERY: &[&str] = &[
    "gpio-server",
    "i2c-server",
    "spi-server",
    "haptics-server",
    "rgb-led-server",
    "emmc",
    "fs-server",
    "usb-server",
    "crypto-server",
    "security-server",
    "mass-storage-server",
    "power-manager-server",
    "dma-server",
    "gui-server",
    "gui-app-control-center",
    "gui-app-keyboard",
    "gui-app-recovery",
    "recovery-worker",
    "gui-app-file-browser",
];

// order here can help optimize boot if it's ordered by dependency
const DEFAULT_SERVICES_NORMAL: &[&str] = &[
    "gpio-server",
    "i2c-server",
    "spi-server",
    "haptics-server",
    "rgb-led-server",
    "nfc-server",
    "emmc",
    "fs-server",
    "settings-server",
    "usb-server",
    "crypto-server",
    "mass-storage-server",
    "mass-storage-emulation",
    "power-manager-server",
    "dma-server",
    "bt-server",
    "update-server",
    "security-server",
    "quantum-link-server",
    "backup-server",
    "fido",
    "ctap-hid",
    "legacy-hid",
    "keycard-server",
    "app-manager-server",
    "camera-server",
    "gui-server",
    "gui-app-control-center",
    "gui-app-lock-screen",
    "gui-app-keyboard",
    "gui-app-launcher",
    "gui-app-switcher",
];

// Built-in apps launched on demand by app-manager rather than run as services,
// on hardware and hosted alike; DEV_APPS are the dev-only additions on top.
const DEFAULT_APPS_NORMAL: &[&str] = &[
    "gui-app-alerts",
    "gui-app-authenticator",
    "gui-app-bitcoin",
    "gui-app-emu-flux-server",
    "gui-app-file-browser",
    "gui-app-onboarding",
    "gui-app-qr-scanner",
    "gui-app-security-keys",
    "gui-app-seed-vault",
    "gui-app-settings",
];

const DEV_APPS: &[&str] = &[
    "gui-app-crypto-perf",
    "gui-app-entropy-test",
    "gui-app-file-picker-test",
    "gui-app-image-viewer",
    "gui-app-playground",
    // "gui-app-regulatory",
    "gui-app-system-actions",
    "gui-app-update-test",
];

const DEFAULT_FLUX_APPS_NORMAL: &[&str] = &["app-flux-ethereum", "app-flux-solana"];

const DEFAULT_FLUX_APPS_HOSTED: &[&str] = &[/*"app-flux-ethereum"*/];

// Services whose exit should take the whole hosted system down with them. A service
// belongs here when it runs a real hosted server (its `main` keeps listening), so an
// exit means a crash rather than the intended hosted no-op. Services whose hosted
// `main` returns immediately (hardware stubs gated on `keyos`) stay out, or the sim
// would tear itself down at boot.
pub const SYSTEM_SERVICES_HOSTED: &[&str] = &[
    "app-manager-server",
    "backup-server",
    "bt-server",
    "camera-server",
    "crypto-server",
    "fido",
    "fs-server",
    "gui-server",
    "haptics-server",
    "legacy-hid",
    "log-file",
    "nfc-server",
    "power-manager-server",
    "quantum-link-server",
    "rgb-led-server",
    "security-server",
    "settings-server",
    "update-server",
    "simulator",
    "simulator-cli",
];

const DEFAULT_SERVICES_HOSTED: &[&str] = &[
    // No hosted implementation; their `main` exits immediately (so they stay out of
    // SYSTEM_SERVICES_HOSTED), built only so clients referencing their messages pass
    // manifest validation.
    "dma-server",
    "emmc",
    "gpio-server",
    "i2c-server",
    "keycard-server",
    "mass-storage-server",
    "spi-server",
    "usb-server",
    // The UI apps kept as always-running services.
    "gui-app-control-center",
    "gui-app-lock-screen",
    "gui-app-keyboard",
    "gui-app-launcher",
    "gui-app-switcher",
    // "recovery-worker",
];

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct XtaskArgs {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build the at91bootstrap bootloader
    BuildBootloader(BootloaderBuildArgs),
    /// Build a tiny boot image for factory charging the device
    BuildChargeBoot,
    /// Build service+app+kernel binary images
    Build {
        /// Don't sign the image (used for CI)
        #[arg(long)]
        dont_sign: bool,
        #[command(flatten)]
        build_args: BuildArgs,
    },
    /// Build one app crate into a signed, sideloadable bundle (no firmware image).
    BuildApp {
        /// App crate to bundle (e.g. app-flux-ethereum)
        app: String,
        /// Output parent directory; the bundle is written to <OUT>/<app-id-hex>/
        /// (default: <target-root>/app-bundles)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Developer signing key's cosign2.toml (e.g. from the SDK `foundation cert gen`, whose
        /// publisher certificate is installed on the device). Defaults to the repo key, which is
        /// not an allowed publisher (the bundle uploads but will not launch).
        #[arg(long = "cosign2", value_name = "COSIGN2_TOML")]
        cosign2: Option<PathBuf>,
        /// Build for the simulator (hosted) instead of hardware; leaves the bundle unsigned.
        #[arg(long)]
        hosted: bool,
        /// Also pack the bundle into a single <APP>.app archive, for installing the app from the
        /// device's Settings instead of over usb-debug. A hosted bundle is unsigned and its app.elf
        /// is a host binary, so the device could only reject an archive of one.
        #[arg(long, conflicts_with = "hosted")]
        archive: bool,
    },
    /// Build a full flash-able firmware image, combining the bootloader, the recovery and normal images.
    /// Run the following first (in this order):
    ///     - build-bootloader
    ///     - build --recovery
    ///     - build
    #[command(verbatim_doc_comment)]
    BuildFirmwareImage {
        #[command(flatten)]
        samba_crypt_args: SambaCryptArgs,
    },
    /// Build all of the above, resulting in full flash-able firmware image
    BuildAll {
        #[command(flatten)]
        bootloader_args: BootloaderBuildArgs,
        #[arg(long)]
        dont_sign: bool,
        #[command(flatten)]
        build_args: BuildArgs,
        #[command(flatten)]
        samba_crypt_args: SambaCryptArgs,
    },
    /// Build and run a services+kernel image
    Run {
        /// GDB command to use to run on hardware
        #[arg(long, default_value = "arm-none-eabi-gdb")]
        gdb: String,
        #[command(flatten)]
        build_args: BuildArgs,
    },
    /// Generate a release tarball from a manifest file.
    GenerateRelease { manifest_file: PathBuf, output_path: PathBuf },
    /// Check crates against both targets (armv7a-unknown-xous-elf and host)
    Check {
        /// Specific crates to check. If not provided, all workspace crates will be checked.
        #[arg(value_name = "CRATE")]
        crates: Vec<String>,
    },
    /// Build and annotate the public KeyOS API rustdoc site
    DocsApi(docs_api::DocsApiArgs),
    /// Run hardware-backed test helpers
    Test(tests::TestArgs),
    /// Flash (parts of) the boot.bin file to the device using sam-ba
    Flash(FlashArgs),
    /// Dump flash contents to a file using sam-ba
    DumpFlash(DumpFlashArgs),
    /// Print the hashes of built artifacts (after a build-all)
    PrintHashes,
    /// Symbolicate a KeyOS backtrace using `addr2line` tool
    Symbolicate(SymbolicateArgs),
    /// Hot-reload a single service in the running simulator without restarting it.
    /// Rebuilds the crate with the hosted profile and signals the kernel to kill and relaunch it.
    #[cfg_attr(not(unix), doc = "Not supported on this platform.")]
    Reload {
        /// Name of the crate to rebuild and reload (e.g. gui-app-emu-flux)
        crate_name: String,
    },
    /// Watch a crate's source directory and hot-reload it in the simulator on every change.
    /// Requires `cargo watch` to be installed (`cargo install cargo-watch`).
    #[cfg_attr(not(unix), doc = "Not supported on this platform.")]
    Watch {
        /// Name of the crate to watch and reload (e.g. gui-app-emu-flux)
        crate_name: String,
    },
}

#[derive(Args, Clone, Default)]
struct BuildArgs {
    /// Services to include in the image. If not set, a default set of services will be included.
    /// Services are processes that start with the kernel.
    /// If set in run mode, all dependencies are added recursively.
    /// Can take the following forms:
    ///    [name]                crate 'name' to be built from local source
    ///    [name@version]        crate 'name' to be fetched from crates.io at the specified version
    ///    [name#URL]            pre-built binary crate of 'name' downloaded from a server at 'URL'
    ///    [path-to-binary]      file path to a prebuilt binary image on local machine.
    ///                          Files in '.' must be specified as './file'
    #[arg(verbatim_doc_comment)]
    services: Vec<String>,
    /// App to build and include in the firmware image.
    /// These are not run by default, but launched by gui-app-launcher instead.
    /// Can be specified multiple times.
    #[arg(long = "app", verbatim_doc_comment, value_name = "APP")]
    apps: Vec<String>,
    /// Flux App to build and include in the firmware image.
    /// These are not run by default, but launched by gui-app-emu-flux instead.
    /// Can be specified multiple times.
    #[arg(long = "flux", verbatim_doc_comment, value_name = "FLUX")]
    flux_apps: Vec<String>,
    /// Build or run in hosted mode, i.e. on the PC. Also known as running the simulator.
    #[arg(long)]
    hosted: bool,
    /// Enable debug logging in the loader
    #[arg(long)]
    verbose_loader: bool,
    /// Enable debug logging in the kernel
    #[arg(long)]
    verbose_kernel: bool,
    /// Enable kernel UART debug shell and UART serial logging in production firmware builds.
    ///
    /// This adds the `log-serial` kernel feature and includes `log-serial`.
    #[arg(long, conflicts_with = "hosted")]
    log_serial: bool,
    /// Enable USB debug protocol service in production firmware builds.
    ///
    /// This includes `usb-debug`.
    #[arg(long, conflicts_with = "hosted")]
    log_usb_debug: bool,
    /// Write logs to files on the external USB drive.
    #[arg(long, conflicts_with = "hosted")]
    log_usb_file: bool,
    /// Enable SystemView for this run. This will build the kernel with SystemView support.
    /// The kernel will wait for SystemView recorder to connect before proceeding with the boot process.
    #[arg(long)]
    with_systemview: bool,
    #[arg(long)]
    integration_test: bool,
    /// We're building a recovery OS image
    #[arg(long)]
    is_recovery: bool,
    // we're building in CI. don't use incremental builds, etc.
    #[arg(long)]
    ci: bool,
    /// Disable incremental compilation for reproducible builds.
    #[arg(long)]
    reproducible: bool,
    /// Disables serial logging output in production firmware. Implies --reproducible.
    /// Use `--log-serial` and/or `--log-usb-debug` to re-enable serial/USB debug logging in production
    /// builds. Internal flash file logging remains enabled.
    #[arg(
        long,
        conflicts_with = "hosted",
        conflicts_with = "verbose_kernel",
        conflicts_with = "verbose_loader",
        conflicts_with = "with_systemview"
    )]
    production_firmware: bool,
}

impl BuildArgs {
    pub fn with_recovery(self, is_recovery: bool) -> Self { Self { is_recovery, ..self } }
}

/// target triple for KeyOS builds
pub(crate) const TARGET_TRIPLE_KEYOS: &str = "armv7a-unknown-xous-elf";

fn main() {
    let args = XtaskArgs::parse();

    match args.command {
        Commands::BuildBootloader(args) => build_keyos_boot(args),
        Commands::BuildChargeBoot => build_charge_boot(),
        Commands::Build { build_args, dont_sign } => {
            build(build_args, dont_sign);
        }
        Commands::BuildApp { app, out, cosign2, hosted, archive } => {
            let builder = if hosted { Builder::hosted() } else { Builder::hardware() };
            let out = out.unwrap_or_else(|| builder.get_target_root().join("app-bundles"));
            let bundle = builder.build_app(&app, &out, cosign2);
            println!();
            println!("App bundle ready at {}", bundle.dir.display());
            println!("Sideload it (device unlocked, Developer Mode on) with:");
            println!("    passport-drive load_app {}", bundle.dir.display());
            if archive {
                pack_app_archive(&bundle, &out.join(app_archive::archive_file_name(&app)));
            }
        }
        Commands::BuildFirmwareImage { samba_crypt_args } => {
            create_boot_image(samba_crypt_args);
        }
        Commands::BuildAll { build_args, dont_sign, bootloader_args, samba_crypt_args } => {
            build_keyos_boot(bootloader_args);
            build(build_args.clone().with_recovery(true), dont_sign);
            build(build_args, dont_sign);
            create_boot_image(samba_crypt_args);
        }
        Commands::Run { gdb, mut build_args } => {
            process_services(&mut build_args);
            let built = Builder::new(build_args).build(SigningMode::Developer);
            built.run(&gdb);
        }
        Commands::GenerateRelease { manifest_file, output_path } => {
            release_generator::generate_release(&manifest_file, &output_path).unwrap();
        }
        Commands::Check { crates } => {
            check_crates(crates);
        }
        Commands::DocsApi(args) => {
            if let Err(err) = docs_api::run(args) {
                eprintln!("Error: {err:#}");
                std::process::exit(1);
            }
        }
        Commands::Test(args) => {
            if let Err(err) = tests::run(args) {
                eprintln!("Error: {err:#}");
                std::process::exit(1);
            }
        }
        Commands::Flash(flash_args) => flash_firmware(flash_args).unwrap(),
        Commands::DumpFlash(dump_flash_args) => dump_flash(dump_flash_args).unwrap(),
        Commands::PrintHashes => print_hashes(),
        Commands::Symbolicate(args) => {
            symbolicate::run_symbolicate(args).unwrap();
        }
        Commands::Reload { crate_name } => hot_reload::reload_service(crate_name),
        Commands::Watch { crate_name } => hot_reload::watch_service(crate_name),
    }
}

/// Pack a built bundle into an app archive the device can install from a USB drive.
fn pack_app_archive(bundle: &BundledApp, archive_path: &std::path::Path) {
    let report = app_archive::pack_bundle(&bundle.dir, archive_path, &bundle.hashed_files)
        .unwrap_or_else(|e| panic!("Could not pack {}: {e}", bundle.dir.display()));
    println!();
    println!(
        "App archive ready at {} ({} entries, {} bytes from {} bytes of bundle)",
        report.archive_path.display(),
        report.entries,
        report.archive_bytes,
        report.bundle_bytes
    );
    println!("Copy it to a USB drive and install it from Settings > Apps on the device.");
}

fn build(mut build_args: BuildArgs, dont_sign: bool) {
    let is_recovery = build_args.is_recovery;
    process_services(&mut build_args);
    let signing_mode = if dont_sign { SigningMode::None } else { SigningMode::Developer };
    Builder::new(build_args).build(signing_mode).build_combined_image(
        &Builder::images_path().join(if is_recovery { RECOVERY_IMAGE } else { APP_IMAGE }),
        signing_mode,
        KEYOS_VERSION,
    );
}

fn process_services(build_args: &mut BuildArgs) {
    let mut mandatory_services: Vec<String> =
        if build_args.hosted { &MANDATORY_SYSTEM_SERVICES_HOSTED } else { &MANDATORY_SYSTEM_SERVICES_HW }
            .iter()
            .map(|s| s.to_string())
            .collect();

    let mut add_service = |service: &str| {
        if !mandatory_services.iter().any(|s| s == service) {
            mandatory_services.push(service.to_string());
        }
    };

    if !build_args.hosted {
        // Keep internal file logging enabled for all hardware builds.
        add_service(LOGGING_SERVICE_FILE);

        // In non-production firmware, auto-enable serial logging outputs.
        if !build_args.production_firmware {
            build_args.log_serial = true;
            build_args.log_usb_debug = true;
        }

        if build_args.log_serial {
            add_service(LOGGING_SERVICE_SERIAL);
        }
        // Only auto-include usb-debug when using default services (which
        // include gui-server). Explicit service lists (e.g. sys-benchmark)
        // may not have gui-server, and usb-debug can't function without it.
        if build_args.log_usb_debug && build_args.services.is_empty() {
            add_service(LOGGING_SERVICE_USB_DEBUG);
        }
        if build_args.log_usb_file {
            add_service(LOGGING_SERVICE_USB_FILE);
        }
    }
    if build_args.services.is_empty() {
        let additional_crates: Vec<&str> = if build_args.hosted {
            if build_args.apps.is_empty() {
                build_args.apps = DEFAULT_APPS_NORMAL.iter().map(|s| s.to_string()).collect();
                build_args.apps.extend(DEV_APPS.iter().map(|s| s.to_string()));
            }
            if build_args.flux_apps.is_empty() {
                build_args.flux_apps = DEFAULT_FLUX_APPS_HOSTED.iter().map(|s| s.to_string()).collect();
            }
            SYSTEM_SERVICES_HOSTED.iter().chain(DEFAULT_SERVICES_HOSTED.iter()).copied().collect()
        } else if build_args.is_recovery {
            DEFAULT_SERVICES_RECOVERY.to_vec()
        } else {
            if build_args.apps.is_empty() {
                build_args.apps = DEFAULT_APPS_NORMAL.iter().map(|s| s.to_string()).collect();
                if !build_args.production_firmware {
                    build_args.apps.extend(DEV_APPS.iter().map(|s| s.to_string()));
                }
            };
            if build_args.flux_apps.is_empty() {
                build_args.flux_apps = DEFAULT_FLUX_APPS_NORMAL.iter().map(|s| s.to_string()).collect()
            };
            DEFAULT_SERVICES_NORMAL.to_vec()
        };
        build_args.services =
            mandatory_services.into_iter().chain(additional_crates.iter().map(|s| s.to_string())).collect();
    } else {
        for new_crate in mandatory_services.into_iter().rev() {
            if !build_args.services.contains(&new_crate) {
                build_args.services.insert(0, new_crate);
            }
        }
        let mut new_crates = Vec::new();
        for crate_name in build_args.services.clone() {
            for dep_crate in get_crate_os_deps(&crate_name) {
                if !new_crates.contains(&dep_crate) {
                    new_crates.push(dep_crate)
                }
            }
            if !new_crates.contains(&crate_name) {
                new_crates.push(crate_name)
            }
        }
        build_args.services = new_crates;
    }
}

fn check_crates(crates: Vec<String>) {
    use std::process::{Command, Stdio};

    // Crates that only work on specific targets
    const HOST_ONLY_CRATES: &[&str] = &["simulator", "log-hosted", "simulator-cli"];
    const ARM_ONLY_CRATES: &[&str] = &["log-serial", "usb-debug"];

    let crates_to_check = if crates.is_empty() {
        // Get all the crates that should work on both targets
        let mut all_crates = Vec::new();

        // Add all services
        all_crates.extend(MANDATORY_SYSTEM_SERVICES_HW.iter().map(|s| s.to_string()));
        all_crates.extend(MANDATORY_SYSTEM_SERVICES_HOSTED.iter().map(|s| s.to_string()));
        all_crates.extend(DEFAULT_SERVICES_RECOVERY.iter().map(|s| s.to_string()));
        all_crates.extend(DEFAULT_SERVICES_NORMAL.iter().map(|s| s.to_string()));
        all_crates.extend(DEFAULT_SERVICES_HOSTED.iter().map(|s| s.to_string()));
        all_crates.extend(SYSTEM_SERVICES_HOSTED.iter().map(|s| s.to_string()));

        // Add all apps
        all_crates.extend(DEFAULT_APPS_NORMAL.iter().map(|s| s.to_string()));
        all_crates.extend(DEV_APPS.iter().map(|s| s.to_string()));

        // Remove duplicates
        all_crates.sort();
        all_crates.dedup();

        all_crates
    } else {
        crates
    };

    // Group crates by target compatibility
    let mut arm_crates = Vec::new();
    let mut host_crates = Vec::new();

    for crate_name in &crates_to_check {
        let is_host_only = HOST_ONLY_CRATES.contains(&crate_name.as_str());
        let is_arm_only = ARM_ONLY_CRATES.contains(&crate_name.as_str());

        if !is_host_only {
            arm_crates.push(crate_name);
        }
        if !is_arm_only {
            host_crates.push(crate_name);
        }
    }

    let mut children = Vec::new();

    // Spawn ARM target check
    if !arm_crates.is_empty() {
        println!("Checking {} crates for ARM target...", arm_crates.len());
        let mut cmd = Command::new(cargo());
        cmd.arg("check")
            .arg("--target")
            .arg(TARGET_TRIPLE_KEYOS)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for crate_name in &arm_crates {
            cmd.arg("-p").arg(crate_name);
        }

        let child = cmd.spawn().expect("Failed to spawn cargo check for ARM");
        children.push(("ARM", child));
    }

    // Spawn host target check
    if !host_crates.is_empty() {
        println!("Checking {} crates for simulator target...", host_crates.len());
        let mut cmd = Command::new(cargo());
        cmd.arg("check").stdout(Stdio::piped()).stderr(Stdio::piped());

        for crate_name in &host_crates {
            cmd.arg("-p").arg(crate_name);
        }

        let child = cmd.spawn().expect("Failed to spawn cargo check for host");
        children.push(("host", child));
    }

    // Poll children and collect results
    let mut failed = false;
    for (target, child) in children {
        let output = child.wait_with_output().expect(&format!("Failed to wait for {} check", target));

        if !output.status.success() {
            failed = true;
            eprintln!("\n{} target check failed!", target);
            eprintln!("stdout:\n{}", String::from_utf8_lossy(&output.stdout));
            eprintln!("stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        }
    }

    if failed {
        std::process::exit(1);
    }
}
