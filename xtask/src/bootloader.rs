// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs;
use std::path::Path;
use std::process::Command;

use clap::Args;
use sha2::Digest;

use crate::{
    builder::cargo, project_root, utils::*, BOOTLOADER_IMAGE, BOOTLOADER_IMAGE_CIPHER, TARGET_TRIPLE_KEYOS,
};

const EXTRA_ENTROPY_MARKER: [u8; 32] = *b"extra_entropy_replaced_by_xtask_";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BootloaderType {
    KeyOs,
    Charge,
}

#[derive(Args, Default)]
pub struct BootloaderBuildArgs {
    /// Set the EXTRA_ENTROPY global variable to this value.
    /// Format: 32 byte hex string
    #[arg(long, default_value = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f")]
    extra_entropy: String,
    /// Build bootloader in production mode (adds checks on tamper, signing keys, etc.)
    #[arg(long)]
    production_bootloader: bool,
}

#[derive(Args)]
pub struct SambaCryptArgs {
    /// If bootloader encryption is to be used, path to the sam-ba-cipher helper binary.
    #[arg(long)]
    pub samba_cipher_tool: Option<String>,
    #[arg(long)]
    samba_cipher_license: Option<String>,
    #[arg(long)]
    samba_cipher_license_key: Option<String>,
    #[arg(long)]
    samba_customer_key: Option<String>,
    #[arg(long)]
    samba_password_file: Option<String>,
}

impl SambaCryptArgs {
    #[allow(dead_code)]
    pub fn no_encryption() -> Self {
        Self {
            samba_cipher_tool: None,
            samba_cipher_license: None,
            samba_cipher_license_key: None,
            samba_customer_key: None,
            samba_password_file: None,
        }
    }
}

pub fn build_keyos_boot(args: BootloaderBuildArgs) {
    let bootloader_bytes = build_at91bootstrap(args, BootloaderType::KeyOs);
    let images_path = project_root().join("target").join(TARGET_TRIPLE_KEYOS).join("release").join("images");
    fs::create_dir_all(&images_path).unwrap();
    fs::write(images_path.join(BOOTLOADER_IMAGE), bootloader_bytes).expect("write at91bootstrap bootloader");
}

pub fn build_at91bootstrap(args: BootloaderBuildArgs, bl_type: BootloaderType) -> Vec<u8> {
    // Check that armv7a-none-eabi target is installed
    if !is_target_installed("armv7a-none-eabi") {
        eprintln!("Target armv7a-none-eabi is not installed.");
        eprintln!("Run:");
        eprintln!();
        eprintln!("rustup target add armv7a-none-eabi");
        eprintln!();
        eprintln!("to install it.");
        panic!("armv7a-none-eabi target is not installed");
    }

    // 0. Make the rust part of the bootloader

    let mut command = Command::new(cargo());
    let package_name = match bl_type {
        BootloaderType::KeyOs => "keyos-boot",
        BootloaderType::Charge => "charge-boot",
    };
    command.current_dir(project_root());
    command.env("SOURCE_DATE_EPOCH", GIT_TIMESTAMP.clone());
    command.env("RUSTFLAGS", "-C link-arg=-fuse-ld=arm-none-eabi-ld -C target-feature=+thumb-mode -Z location-detail=none -Z fmt-debug=none");
    command.args(["build", "--profile", "bootloader"]);
    command.args(["--package", package_name]);
    command.args(["--target", "armv7a-none-eabi"]);
    command.args(["-Z", "build-std=panic_abort"]);
    command.args(["-Z", "build-std-features=panic_immediate_abort"]);
    if args.production_bootloader {
        command.args(["--features", "production"]);
    }

    println!("Building boot rust part: cargo: {command:?}");

    let status = command.status().expect("Running Cargo failed");
    if !status.success() {
        panic!("Building rust part of bootloader failed");
    }

    let at91bootstrap_dir = project_root().join("boot/at91bootstrap");
    // 1. Clean at91bootstrap build directory
    let status = Command::new("make")
        .current_dir(&at91bootstrap_dir)
        .args(["mrproper"])
        .status()
        .expect("run make mrproper at at91bootstrap");
    if !status.success() {
        panic!("make mproper failed");
    }

    // 2. Copy ATSAMA5D28 SiP config
    fs::copy(
        project_root().join("scripts").join("sama5d28_sip_img"),
        at91bootstrap_dir.join("configs").join("sama5d27_som1_sd_image_defconfig"),
    )
    .expect("copy at91bootstrap config");

    // 3. Configure the at91bootstrap
    let status = Command::new("make")
        .current_dir(&at91bootstrap_dir)
        .env("HOSTCC", "cc")
        .args(["sama5d27_som1_sd_image_defconfig"])
        .status()
        .expect("run make sama5d27_som1_sd_image_defconfig at at91bootstrap");
    if !status.success() {
        panic!("make sama5d27_som1_sd_image_defconfig failed");
    }

    // 4. make (builds at91bootstrap binary)
    let mut command = Command::new("make");
    command.env("CROSS_COMPILE", "arm-none-eabi-");
    command.env("HOSTCC", "cc");
    command.env("SOURCE_DATE_EPOCH", GIT_TIMESTAMP.clone());
    command.env("FFI_LIB", package_name.replace('-', "_"));
    command.current_dir(&at91bootstrap_dir);

    let status = command.status().expect("run make at at91bootstrap");
    if !status.success() {
        panic!("make failed");
    }

    // 5. copy the bootloader binary to the images directory
    let bootloader_path = at91bootstrap_dir.join("build").join("binaries").join(BOOTLOADER_IMAGE);

    // 6. Set extra entropy
    let mut bootloader_bytes = fs::read(bootloader_path).expect("Could not read bootloader binary");
    if bl_type == BootloaderType::KeyOs {
        let extra_entropy = hex::decode(args.extra_entropy).expect("Wrong format on extra-entropy");
        set_extra_entropy(&mut bootloader_bytes, &extra_entropy);
    }

    let bootloader_hash = sha2::Sha256::digest(&bootloader_bytes);
    println!("Bootloader hash: {}", hex::encode(&bootloader_hash));
    bootloader_bytes
}

fn set_extra_entropy(bootloader_bytes: &mut [u8], extra_entropy: &[u8]) {
    let extra_entropy_positions: Vec<usize> = bootloader_bytes
        .windows(EXTRA_ENTROPY_MARKER.len())
        .enumerate()
        .filter(|(_, w)| w == &EXTRA_ENTROPY_MARKER)
        .map(|(i, _)| i)
        .collect();
    if extra_entropy_positions.len() == 0 {
        panic!(
            "Could not find EXTRA_ENTROPY variable. Please check if bytes {EXTRA_ENTROPY_MARKER:02x?} are present in atbootstrap91-ffi"
        )
    } else if extra_entropy_positions.len() > 1 {
        panic!(
            "EXTRA_ENTROPY found more than once. Please check if the variable is inlined, duplicated or similar."
        )
    };

    if extra_entropy.len() != EXTRA_ENTROPY_MARKER.len() {
        panic!(
            "Wrong length on extra-entropy: {}, instead of {}",
            extra_entropy.len(),
            EXTRA_ENTROPY_MARKER.len()
        )
    }
    let extra_entropy_position = extra_entropy_positions[0];

    bootloader_bytes[extra_entropy_position..extra_entropy_position + EXTRA_ENTROPY_MARKER.len()]
        .copy_from_slice(&extra_entropy);
    println!(
        "Used a EXTRA_ENTROPY value ({:02x?}...) at offset 0x{extra_entropy_position:08x}.",
        &extra_entropy[..8]
    );
}

pub fn encrypt_bootloader(images_path: &Path, samba_crypt_args: SambaCryptArgs) {
    println!("Encrypting the bootloader with `samba-cipher-tool`");

    let bootloader_bytes =
        fs::read(images_path.join(BOOTLOADER_IMAGE)).expect("Could not read bootloader binary");
    if bootloader_bytes.windows(EXTRA_ENTROPY_MARKER.len()).find(|w| w == &EXTRA_ENTROPY_MARKER).is_some() {
        panic!(
            "Trying to encrypt a bootloader that still uses the default entropy. Use the --extra-entropy parameter"
        );
    }

    // Skip customer key generation - use existing files from FINAL-SECRETS
    println!("Using existing customer key files from FINAL-SECRETS directory");

    let default_tool_name = "secure-sam-ba-cipher.py".to_string();

    let samba_tool_dir = std::env::var("SAMBA_PYTHON")
        .map(|python_path| {
            // SAMBA_PYTHON is like: /path/to/secure-sam-ba-cipher-3.7/venv/bin/python
            // We want: /path/to/secure-sam-ba-cipher-3.7
            std::path::Path::new(&python_path)
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf()
        })
        .unwrap_or_else(|_| {
            let tool_path = samba_crypt_args
                .samba_cipher_tool
                .as_ref()
                .expect("Missing --samba-cipher-tool when encryption is enabled");
            std::path::Path::new(tool_path).parent().expect("Invalid --samba-cipher-tool path").to_path_buf()
        });

    let samba_tool_name: String = samba_crypt_args
        .samba_cipher_tool
        .as_ref()
        .map(|p| std::path::Path::new(p).file_name().unwrap().to_string_lossy().to_string())
        .unwrap_or(default_tool_name);

    let samba_cipher_license =
        samba_crypt_args.samba_cipher_license.clone().expect("Missing --samba-cipher-license");

    let samba_customer_key =
        samba_crypt_args.samba_customer_key.clone().expect("Missing --samba-customer-key");

    // Encrypt the bootloader image.
    let args = vec![
        samba_tool_name,
        "bootstrap".to_string(),
        "-d".to_string(),
        "sama5d2x".to_string(),
        "-l".to_string(),
        samba_cipher_license,
        "-k".to_string(),
        samba_customer_key,
        "-i".to_string(),
        std::env::current_dir()
            .unwrap()
            .join(images_path)
            .join(BOOTLOADER_IMAGE)
            .to_str()
            .unwrap()
            .to_string(),
        "-o".to_string(),
        "boot".to_string(),
        "-b".to_string(),
        "true".to_string(),
    ];
    let python_cmd = std::env::var("SAMBA_PYTHON").unwrap_or_else(|_| "python3".to_string());

    let output =
        Command::new(&python_cmd).args(&args).current_dir(&samba_tool_dir).output().unwrap_or_else(|e| {
            panic!(
                "Failed to execute SAM-BA cipher tool with command '{}': {}\nArgs: {:?}\nDirectory: {:?}",
                python_cmd, e, args, samba_tool_dir
            );
        });
    if !output.status.success() {
        panic!("Failed to generate the bootloader image:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    // Move the encrypted bootloader file from SAM-BA tool directory to images directory
    let samba_output_file = samba_tool_dir.join("boot_sama5d2x.cip");
    let target_output_file = project_root()
        .join("target")
        .join(TARGET_TRIPLE_KEYOS)
        .join("release")
        .join("images")
        .join(BOOTLOADER_IMAGE_CIPHER);

    if samba_output_file.exists() {
        std::fs::copy(&samba_output_file, &target_output_file).unwrap_or_else(|e| {
            panic!(
                "Failed to copy encrypted bootloader from {:?} to {:?}: {}",
                samba_output_file, target_output_file, e
            );
        });
        println!("Encrypted bootloader copied to: {:?}", target_output_file);
    } else {
        panic!("SAM-BA cipher tool did not create expected output file: {:?}", samba_output_file);
    }
}
