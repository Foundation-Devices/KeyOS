// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs;
use std::mem::size_of;
use std::ops::Range;
use std::path::Path;
use std::process::Command;

use clap::Args;
use sha2::Digest;

use crate::{
    builder::cargo, project_root, utils::*, BOOTLOADER_IMAGE, BOOTLOADER_IMAGE_CIPHER, TARGET_TRIPLE_KEYOS,
};

const EXTRA_ENTROPY_MARKER: [u8; 32] = *b"extra_entropy_replaced_by_xtask_";
const DEFAULT_EXTRA_ENTROPY: [u8; 32] =
    hex_literal::hex!("000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f");
const BOOTLOADER_SOURCE_DATE_EPOCH_FILE: &str = "boot/keyos-boot/SOURCE_DATE_EPOCH";
pub(crate) const BOOTLOADER_HASH_RECORD: &str = "boot.hashes.json";
const BOOTLOADER_SRAM_SIZE: usize = 64 * 1024;
const BOOTLOADER_RAW_MAX_SIZE: usize = BOOTLOADER_SRAM_SIZE - SECURE_BOOT_CMAC_SIZE;
// Keep this synchronized with BOOTLOADER_SIZE_IDX in os/recovery-worker/src/system_info.rs.
const BOOTLOADER_SIZE_VECTOR: Range<usize> = 5 * size_of::<u32>()..6 * size_of::<u32>();
const SECURE_BOOT_AES_BLOCK_SIZE: usize = 16;
const SECURE_BOOT_CMAC_SIZE: usize = 16;

#[derive(serde::Deserialize, serde::Serialize)]
struct BootloaderHashRecord {
    raw_sha256: String,
    on_device_sha256: String,
}

pub(crate) struct HistoricalBootloaderResult {
    pub(crate) raw_sha256: String,
    pub(crate) on_device_sha256: String,
    pub(crate) raw_size: usize,
    pub(crate) secure_boot_sram_size: usize,
}

/// Normalize and hash a plaintext bootloader built from historical source.
#[derive(Args)]
pub struct HashBootloaderArgs {
    /// Historical boot.bin built with the original EXTRA_ENTROPY marker left in place.
    #[arg(value_name = "BOOT_BIN")]
    bootloader: std::path::PathBuf,
    /// Directory in which to write the normalized boot.bin and boot.hashes.json.
    #[arg(long, value_name = "DIR")]
    output_dir: std::path::PathBuf,
}

pub(crate) struct BuiltBootloader {
    pub(crate) bytes: Vec<u8>,
    raw_hash: [u8; 32],
    on_device_hash: Option<[u8; 32]>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BootloaderType {
    KeyOs,
    Charge,
}

#[derive(Args, Default)]
pub struct BootloaderBuildArgs {
    /// Set the EXTRA_ENTROPY global variable to this value.
    /// Format: 32 byte hex string
    #[arg(long, default_value_t = hex::encode(DEFAULT_EXTRA_ENTROPY))]
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
    let bootloader = build_at91bootstrap(args, BootloaderType::KeyOs);
    let images_path = project_root().join("target").join(TARGET_TRIPLE_KEYOS).join("release").join("images");
    fs::create_dir_all(&images_path).unwrap();
    fs::write(images_path.join(BOOTLOADER_IMAGE), &bootloader.bytes).expect("write at91bootstrap bootloader");

    let hash_record = BootloaderHashRecord {
        raw_sha256: hex::encode(bootloader.raw_hash),
        on_device_sha256: hex::encode(
            bootloader.on_device_hash.expect("KeyOS bootloader has on-device hash"),
        ),
    };
    let mut encoded_record = serde_json::to_string_pretty(&hash_record).expect("serialize bootloader hashes");
    encoded_record.push('\n');
    fs::write(images_path.join(BOOTLOADER_HASH_RECORD), encoded_record)
        .expect("write bootloader hash record");
}

/// Hash a bootloader produced by an older KeyOS checkout without changing that checkout.
///
/// Historical xtask versions can leave [`EXTRA_ENTROPY_MARKER`] in the emitted image when passed
/// that value explicitly. It uniquely identifies the secret slot even though the public entropy
/// value also appears elsewhere in the bootloader. Normalize the slot to [`DEFAULT_EXTRA_ENTROPY`]
/// before calculating either hash, then use the same SRAM model as current release builds.
pub fn hash_historical_bootloader(args: HashBootloaderArgs) -> anyhow::Result<()> {
    let result = write_historical_bootloader(&args.bootloader, &args.output_dir)?;

    println!("Historical bootloader:       {}", args.bootloader.display());
    println!("Normalized bootloader:       {}", args.output_dir.join(BOOTLOADER_IMAGE).display());
    println!("Raw bootloader size:         {}", result.raw_size);
    println!("Secure Boot SRAM size:       {}", result.secure_boot_sram_size);
    println!("Raw bootloader SHA256:       {}", result.raw_sha256);
    println!("On-device bootloader SHA256: {}", result.on_device_sha256);
    Ok(())
}

pub(crate) fn write_historical_bootloader(
    input_path: &Path,
    output_dir: &Path,
) -> anyhow::Result<HistoricalBootloaderResult> {
    let input = fs::read(input_path)
        .map_err(|error| anyhow::anyhow!("could not read {}: {error}", input_path.display()))?;
    let (bootloader_bytes, extra_entropy_offset) = normalize_historical_bootloader(input)?;
    let raw_hash: [u8; 32] = sha2::Sha256::digest(&bootloader_bytes).into();
    let on_device_hash = on_device_bootloader_hash(&bootloader_bytes, extra_entropy_offset);
    let secure_boot_sram_size = secure_boot_size(bootloader_bytes.len());

    fs::create_dir_all(output_dir)
        .map_err(|error| anyhow::anyhow!("could not create {}: {error}", output_dir.display()))?;
    let output_bootloader = output_dir.join(BOOTLOADER_IMAGE);
    fs::write(&output_bootloader, &bootloader_bytes)
        .map_err(|error| anyhow::anyhow!("could not write {}: {error}", output_bootloader.display()))?;

    let record = BootloaderHashRecord {
        raw_sha256: hex::encode(raw_hash),
        on_device_sha256: hex::encode(on_device_hash),
    };
    let mut encoded_record = serde_json::to_string_pretty(&record)?;
    encoded_record.push('\n');
    let record_path = output_dir.join(BOOTLOADER_HASH_RECORD);
    fs::write(&record_path, encoded_record)
        .map_err(|error| anyhow::anyhow!("could not write {}: {error}", record_path.display()))?;

    Ok(HistoricalBootloaderResult {
        raw_sha256: record.raw_sha256,
        on_device_sha256: record.on_device_sha256,
        raw_size: bootloader_bytes.len(),
        secure_boot_sram_size,
    })
}

pub fn build_at91bootstrap(args: BootloaderBuildArgs, bl_type: BootloaderType) -> BuiltBootloader {
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
    let source_date_epoch = match bl_type {
        BootloaderType::KeyOs => bootloader_source_date_epoch(),
        BootloaderType::Charge => GIT_TIMESTAMP.clone(),
    };
    println!("Bootloader SOURCE_DATE_EPOCH: {source_date_epoch}");
    command.current_dir(project_root());
    command.env("SOURCE_DATE_EPOCH", &source_date_epoch);
    command.env("RUSTFLAGS", "-C link-arg=-fuse-ld=arm-none-eabi-ld -C target-feature=+thumb-mode -Z location-detail=none -Z fmt-debug=none");
    command.args(["build", "--profile", "bootloader"]);
    command.args(["--package", package_name]);
    command.args(["--target", "armv7a-none-eabi"]);
    command.args(["-Z", "build-std=core"]);
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
    command.env("SOURCE_DATE_EPOCH", &source_date_epoch);
    command.env("FFI_LIB", package_name.replace('-', "_"));
    if bl_type == BootloaderType::KeyOs {
        // Secure SAM-BA adds a 16-byte CMAC after AES block padding. Reserve that space in SRAM.
        // at91bootstrap also doubles this value for its static-RAM ceiling, so this conservatively
        // tightens that separate limit by 32 bytes. The current image is well below either limit.
        command.arg(format!("BOOTSTRAP_MAXSIZE={BOOTLOADER_RAW_MAX_SIZE}"));
    }
    command.current_dir(&at91bootstrap_dir);

    let status = command.status().expect("run make at at91bootstrap");
    if !status.success() {
        panic!("make failed");
    }

    // 5. copy the bootloader binary to the images directory
    let bootloader_path = at91bootstrap_dir.join("build").join("binaries").join(BOOTLOADER_IMAGE);

    // 6. Set extra entropy
    let mut bootloader_bytes = fs::read(bootloader_path).expect("Could not read bootloader binary");
    let extra_entropy_offset = if bl_type == BootloaderType::KeyOs {
        let extra_entropy = hex::decode(args.extra_entropy).expect("Wrong format on extra-entropy");
        Some(set_extra_entropy(&mut bootloader_bytes, &extra_entropy))
    } else {
        None
    };

    let secure_boot_size = secure_boot_size(bootloader_bytes.len());
    assert!(
        bl_type != BootloaderType::KeyOs || secure_boot_size <= BOOTLOADER_SRAM_SIZE,
        "Secure Boot image size {secure_boot_size} exceeds the {BOOTLOADER_SRAM_SIZE}-byte SRAM window"
    );

    let raw_hash: [u8; 32] = sha2::Sha256::digest(&bootloader_bytes).into();
    println!("Raw bootloader SHA256: {}", hex::encode(raw_hash));
    let on_device_hash =
        extra_entropy_offset.map(|offset| on_device_bootloader_hash(&bootloader_bytes, offset));
    if let Some(hash) = on_device_hash {
        println!("On-device bootloader SHA256: {}", hex::encode(hash));
    }
    BuiltBootloader { bytes: bootloader_bytes, raw_hash, on_device_hash }
}

fn bootloader_source_date_epoch() -> String {
    if let Ok(source_date_epoch) = std::env::var("KEYOS_SOURCE_DATE_EPOCH") {
        source_date_epoch.parse::<u64>().expect("KEYOS_SOURCE_DATE_EPOCH must be an unsigned integer");
        return source_date_epoch;
    }

    let path = project_root().join(BOOTLOADER_SOURCE_DATE_EPOCH_FILE);
    let value = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("Could not read {}: {error}", path.display()))
        .trim()
        .to_owned();
    value.parse::<u64>().unwrap_or_else(|_| panic!("{} must contain one unsigned integer", path.display()));
    value
}

fn set_extra_entropy(bootloader_bytes: &mut [u8], extra_entropy: &[u8]) -> usize {
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
    println!("Set 32-byte EXTRA_ENTROPY at offset 0x{extra_entropy_position:08x}.");
    extra_entropy_position
}

fn normalize_historical_bootloader(mut bootloader_bytes: Vec<u8>) -> anyhow::Result<(Vec<u8>, usize)> {
    let positions: Vec<usize> = bootloader_bytes
        .windows(EXTRA_ENTROPY_MARKER.len())
        .enumerate()
        .filter(|(_, window)| window == &EXTRA_ENTROPY_MARKER)
        .map(|(position, _)| position)
        .collect();
    anyhow::ensure!(
        positions.len() == 1,
        "expected exactly one historical EXTRA_ENTROPY marker, found {}",
        positions.len()
    );
    let extra_entropy_offset = positions[0];
    bootloader_bytes[extra_entropy_offset..extra_entropy_offset + DEFAULT_EXTRA_ENTROPY.len()]
        .copy_from_slice(&DEFAULT_EXTRA_ENTROPY);
    Ok((bootloader_bytes, extra_entropy_offset))
}

/// Calculate the hash displayed by System Information after the secure bootloader starts recovery.
///
/// Secure SAM-BA pads the plaintext to an AES block, appends a 16-byte CMAC, and rewrites the sixth
/// vector with that total size. Before handing off to recovery, the bootloader replaces its secret
/// entropy with the public [`DEFAULT_EXTRA_ENTROPY`] value and zeroes SRAM from the end of the raw
/// image onward. `boot/at91bootstrap/elf32-littlearm.lds` rejects writable data, which guarantees
/// that the raw image ends at the bootloader's `_etext` scrub boundary. Model those transformations
/// so a local plaintext build can be compared with the hash shown on-device.
fn on_device_bootloader_hash(bootloader_bytes: &[u8], extra_entropy_offset: usize) -> [u8; 32] {
    sha2::Sha256::digest(on_device_bootloader_image(bootloader_bytes, extra_entropy_offset)).into()
}

/// Read the device-comparable hash captured while the entropy slot offset was still known.
///
/// The raw hash in the sidecar binds it to this exact `boot.bin`, preventing a stale build record
/// from being used after the image has been replaced.
pub(crate) fn recorded_on_device_bootloader_hash(
    images_path: &Path,
    bootloader_bytes: &[u8],
) -> anyhow::Result<[u8; 32]> {
    let record_path = images_path.join(BOOTLOADER_HASH_RECORD);
    let record_bytes = fs::read(&record_path)
        .map_err(|error| anyhow::anyhow!("could not read {}: {error}", record_path.display()))?;
    decode_bootloader_hash_record(&record_bytes, bootloader_bytes)
        .map_err(|error| anyhow::anyhow!("{}: {error}", record_path.display()))
}

fn decode_bootloader_hash_record(record_bytes: &[u8], bootloader_bytes: &[u8]) -> anyhow::Result<[u8; 32]> {
    let record: BootloaderHashRecord = serde_json::from_slice(record_bytes)?;

    let actual_raw_hash = hex::encode(sha2::Sha256::digest(bootloader_bytes));
    anyhow::ensure!(
        record.raw_sha256 == actual_raw_hash,
        "hash record does not describe the current {BOOTLOADER_IMAGE}; rebuild the bootloader"
    );

    let hash = hex::decode(&record.on_device_sha256)?;
    hash.try_into().map_err(|_| anyhow::anyhow!("on-device hash is not 32 bytes"))
}

fn secure_boot_size(raw_size: usize) -> usize {
    raw_size
        .checked_next_multiple_of(SECURE_BOOT_AES_BLOCK_SIZE)
        .and_then(|size| size.checked_add(SECURE_BOOT_CMAC_SIZE))
        .expect("bootloader size overflow")
}

fn on_device_bootloader_image(bootloader_bytes: &[u8], extra_entropy_offset: usize) -> Vec<u8> {
    // Secure SAM-BA pads with zeroes only up to the block boundary, never a full PKCS#7-style block.
    let secure_boot_size = secure_boot_size(bootloader_bytes.len());
    assert!(secure_boot_size <= BOOTLOADER_SRAM_SIZE, "Secure Boot image does not fit in SRAM");
    let mut sram_image = bootloader_bytes.to_vec();
    sram_image[extra_entropy_offset..extra_entropy_offset + DEFAULT_EXTRA_ENTROPY.len()]
        .copy_from_slice(&DEFAULT_EXTRA_ENTROPY);
    sram_image[BOOTLOADER_SIZE_VECTOR].copy_from_slice(&(secure_boot_size as u32).to_le_bytes());
    sram_image.resize(secure_boot_size, 0);
    sram_image
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_device_image_models_secure_boot_and_cleanup() {
        let raw = vec![0xa5; 76];

        let image = on_device_bootloader_image(&raw, 40);

        assert_eq!(image.len(), 96); // 76 bytes, padded to 80, plus a 16-byte CMAC.
        assert_eq!(u32::from_le_bytes(image[BOOTLOADER_SIZE_VECTOR].try_into().unwrap()), 96);
        assert_eq!(&image[76..], &[0; 20]);
    }

    #[test]
    fn on_device_image_adds_only_the_cmac_when_already_aligned() {
        let raw = vec![0xa5; 80];

        let image = on_device_bootloader_image(&raw, 40);

        assert_eq!(image.len(), 96); // 80 bytes, already block-aligned, plus a 16-byte CMAC.
        assert_eq!(u32::from_le_bytes(image[BOOTLOADER_SIZE_VECTOR].try_into().unwrap()), 96);
        assert_eq!(&image[80..], &[0; 16]);
    }

    #[test]
    fn on_device_image_restores_public_entropy() {
        let mut raw = vec![0xa5; 80];
        raw[32..64].fill(0x5a);

        let image = on_device_bootloader_image(&raw, 32);

        assert_eq!(image[32..64], DEFAULT_EXTRA_ENTROPY);
    }

    #[test]
    fn bootloader_hash_record_is_bound_to_raw_image() {
        let raw = vec![0xa5; 80];
        let on_device_hash = [0x5a; 32];
        let record = BootloaderHashRecord {
            raw_sha256: hex::encode(sha2::Sha256::digest(&raw)),
            on_device_sha256: hex::encode(on_device_hash),
        };
        let encoded = serde_json::to_vec(&record).unwrap();

        assert_eq!(decode_bootloader_hash_record(&encoded, &raw).unwrap(), on_device_hash);

        let mut different_raw = raw;
        different_raw[0] ^= 1;
        assert!(decode_bootloader_hash_record(&encoded, &different_raw).is_err());
    }

    #[test]
    fn historical_image_marker_is_normalized_before_hashing() {
        let mut historical = vec![0xa5; 80];
        historical[32..64].copy_from_slice(&EXTRA_ENTROPY_MARKER);

        let (normalized, offset) = normalize_historical_bootloader(historical).unwrap();

        assert_eq!(offset, 32);
        assert_eq!(normalized[32..64], DEFAULT_EXTRA_ENTROPY);
        assert!(!normalized.windows(EXTRA_ENTROPY_MARKER.len()).any(|window| window == EXTRA_ENTROPY_MARKER));
    }

    #[test]
    fn historical_image_requires_one_marker() {
        assert!(normalize_historical_bootloader(vec![0xa5; 80]).is_err());

        let mut historical = vec![0xa5; 96];
        historical[0..32].copy_from_slice(&EXTRA_ENTROPY_MARKER);
        historical[64..96].copy_from_slice(&EXTRA_ENTROPY_MARKER);
        assert!(normalize_historical_bootloader(historical).is_err());
    }

    #[test]
    fn maximum_raw_image_fits_secure_boot_sram() {
        assert_eq!(secure_boot_size(BOOTLOADER_RAW_MAX_SIZE), BOOTLOADER_SRAM_SIZE);
        assert!(secure_boot_size(BOOTLOADER_RAW_MAX_SIZE + 1) > BOOTLOADER_SRAM_SIZE);
    }
}
