// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Firmware (boot) image building routines.

use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use fatfs::FileSystem;
use fscommon::StreamSlice;
use hex::ToHex;
use keyos::TOTAL_FLASH_BLOCKS;
use sha2::Digest;
use slint_keyos_platform_build::UI_ICON_SET;

use crate::bootloader::{
    build_at91bootstrap, encrypt_bootloader, recorded_on_device_bootloader_hash, BootloaderBuildArgs,
    SambaCryptArgs,
};
use crate::builder::{project_root, Builder, APP_ICONS_DIR, KEYOS_APPS_DIR};
use crate::system_disk::{render_common_assets, stage_system_volume, SystemVolume};
use crate::{
    APP_IMAGE, BOOTLOADER_IMAGE, BOOTLOADER_IMAGE_CIPHER, BOOT_ASSETS_DIR, RECOVERY_IMAGE,
    TARGET_TRIPLE_KEYOS,
};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

pub const BOOT_IMAGE: &str = "boot.img";

const BOOT_VOLUME_NAME: &str = "KEYOSBOOT";
const SYSTEM_VOLUME_NAME: &str = "PRIME";

const HOSTED_SYSTEM_VOLUME_SIZE: u64 = 128 * MIB;
const HOSTED_USER_VOLUME_SIZE: u64 = 8 * GIB;

pub const SECTOR_SIZE: u64 = 512;
// Leave space for the MBR
const BOOT_PARTITION_START_SECTOR: u32 = 1;
// 256MB minus the one sector we left for the MBR. This way the next partition is nicely aligned.
const BOOT_PARTITION_SIZE_BYTES: u64 = 256 * MIB - SECTOR_SIZE;
const BOOT_PARTITION_SIZE_SECTORS: u32 = (BOOT_PARTITION_SIZE_BYTES / SECTOR_SIZE) as u32;

pub const SYSTEM_PARTITION_START_SECTOR: u32 = BOOT_PARTITION_START_SECTOR + BOOT_PARTITION_SIZE_SECTORS;

// The 0x1000000 (16MB) is an adjustment, so that the FAT table size comes out to exactly 0xff8
// sectors, which (with the default 8 reserved sectors) puts the first data sector to 0x1000.
// This aligns data clusters (of 64 blocks) to flash pages, improving performance (and boot time) by 25%
const SYSTEM_PARTITION_SIZE_BYTES: u64 = 8 * GIB - 0x1000000;
const SYSTEM_PARTITION_SIZE_SECTORS: u32 = (SYSTEM_PARTITION_SIZE_BYTES / SECTOR_SIZE) as u32;

// With the 0x400000 (4MB) adjustment, the first data sector is at sector 0x6400
pub const USER_PARTITION_SIZE_BYTES: u64 = 50 * GIB - 0x400000;
pub const USER_PARTITION_SIZE_SECTORS: u32 = (USER_PARTITION_SIZE_BYTES / SECTOR_SIZE) as u32;
pub const USER_PARTITION_START_SECTOR: u32 = SYSTEM_PARTITION_START_SECTOR + SYSTEM_PARTITION_SIZE_SECTORS;

const _: () = assert!(USER_PARTITION_START_SECTOR + USER_PARTITION_SIZE_SECTORS <= TOTAL_FLASH_BLOCKS as u32);

const RECOVERY_IMAGES: [&str; 8] = [
    "images/background.png",
    "images/battery.png",
    "images/jumbo-circle.png",
    "images/jumbo-download.png",
    "images/shadow-l3__8-16-24-16.png",
    "images/shield-bg__64-64-96-64.png",
    "images/slide-to-bg-dark__27-27-27-27.png",
    "images/slide-to-target-dark__24-24-24-24.png",
];
const RECOVERY_ICONS: [(&str, &[usize]); 25] = [
    ("airlock", &[24]),
    ("alert", &[24, 64, 96]),
    ("arrow-left", &[24]),
    ("arrow-right", &[24]),
    ("backspace", &[24]),
    ("battery", &[24]),
    ("charging", &[24]),
    ("check", &[24, 96]),
    ("check-circle", &[24]),
    ("chevron-left", &[24]),
    ("close", &[24, 96]),
    ("ellipsis", &[24]),
    ("file", &[24, 32]),
    ("filter2", &[24]),
    ("folder", &[24, 32]),
    ("info", &[24]),
    ("off", &[24]),
    ("play", &[24]),
    ("prime", &[24]),
    ("return", &[24]),
    ("search", &[24]),
    ("shield", &[24]),
    ("spinner", &[24]),
    ("unshifted", &[24]),
    ("usb", &[24]),
];

/// Point MBR entry `index` at a freshly FAT32-formatted partition and open it.
fn format_and_open<'a>(
    file: &'a mut File,
    index: u8,
    start_sector: u32,
    sectors: u32,
    label: &str,
    bootable: bool,
) -> anyhow::Result<FileSystem<StreamSlice<&'a mut File>>> {
    println!("Formatting partition #{index}, bootable: {bootable}, start_sector: {start_sector}");
    fatfs_image::set_partition_entry(file, index as usize, start_sector, sectors, bootable)?;
    fatfs_image::format_partition(file, start_sector, sectors, label)?;
    fatfs_image::open_partition(file, index).context("open formatted partition")
}

fn create_boot_partition(file: &mut File, samba_crypt_args: SambaCryptArgs) -> anyhow::Result<()> {
    let images_path = Builder::images_path();
    let should_encrypt_bootloader = samba_crypt_args.samba_cipher_tool.is_some();
    if should_encrypt_bootloader {
        encrypt_bootloader(&images_path, samba_crypt_args);
    }

    let fs = format_and_open(
        file,
        0,
        BOOT_PARTITION_START_SECTOR,
        BOOT_PARTITION_SIZE_SECTORS,
        BOOT_VOLUME_NAME,
        true,
    )
    .context("formatting partition")?;

    if should_encrypt_bootloader {
        fs.root_dir()
            .create_file("boot.cip")?
            .write_all(&fs::read(images_path.join(BOOTLOADER_IMAGE_CIPHER))?)?
    } else {
        fs.root_dir().create_file("boot.bin")?.write_all(&fs::read(images_path.join(BOOTLOADER_IMAGE))?)?;
    }

    fs.root_dir().create_file(RECOVERY_IMAGE)?.write_all(&fs::read(images_path.join(RECOVERY_IMAGE))?)?;

    let bl_assets_dir = fs.root_dir().create_dir(BOOT_ASSETS_DIR)?;
    let bl_source_assets_dir = project_root().join("boot").join("keyos-boot").join("assets").read_dir()?;
    for asset_file in bl_source_assets_dir {
        let file = asset_file?;

        let file_name = file.file_name();
        let file_name = file_name.to_str().unwrap();
        if !file.path().extension().map_or(false, |ext| ext == "raw") {
            continue;
        }

        println!(
            "-> Copying bootloader asset {} -> {}/{}",
            file.path().as_os_str().to_str().unwrap(),
            BOOT_ASSETS_DIR,
            file_name
        );

        bl_assets_dir.create_file(file_name)?.write_all(&fs::read(file.path())?)?;
    }

    let ui_dir_local = project_root().join("ui").join("ui");

    let output_dir =
        project_root().join("target").join(TARGET_TRIPLE_KEYOS).join("release").join("common-boot");
    let images = RECOVERY_IMAGES.iter().map(|img| ui_dir_local.join(img));
    let icons = RECOVERY_ICONS
        .iter()
        .map(|(icon, sizes)| (ui_dir_local.join("icons").join(format!("{icon}.svg")), sizes.to_vec()))
        .collect();

    render_common_assets(&output_dir, images, vec![(UI_ICON_SET, icons)])?;
    fatfs_image::copy_tree_into(&fs, &output_dir, "common").context("bundle common boot assets")?;

    Ok(())
}

fn create_system_partition(file: &mut File) -> anyhow::Result<()> {
    let images_path = Builder::images_path();

    let fs = format_and_open(
        file,
        1,
        SYSTEM_PARTITION_START_SECTOR,
        SYSTEM_PARTITION_SIZE_SECTORS,
        SYSTEM_VOLUME_NAME,
        false,
    )?;

    let keyos_dir = fs.root_dir().create_dir("keyos").context("system: creating `keyos` directory")?;
    keyos_dir.create_file(APP_IMAGE)?.write_all(&fs::read(images_path.join(APP_IMAGE))?)?;

    let target_root = project_root().join("target").join(TARGET_TRIPLE_KEYOS).join("release");
    stage_system_volume(
        &fs,
        &SystemVolume {
            apps_src: &target_root.join(KEYOS_APPS_DIR),
            exclude_app_elf: false,
            common_out: &target_root.join("common"),
            app_icons_src: &target_root.join(APP_ICONS_DIR),
        },
    )?;

    Ok(())
}

fn create_user_partition(file: &mut File) -> anyhow::Result<()> {
    // The user volume is formatted on the device at first boot, so only mark it
    // in the MBR.
    fatfs_image::set_partition_entry(file, 2, USER_PARTITION_START_SECTOR, USER_PARTITION_SIZE_SECTORS, false)
        .context("mark user partition")
}

fn check_images_exist() {
    let images_path = Builder::images_path();
    if fs::metadata(images_path.join(BOOTLOADER_IMAGE)).is_err() {
        panic!("The {BOOTLOADER_IMAGE} file is missing, have you ran cargo xtask build-bootloader?");
    }
    if fs::metadata(images_path.join(APP_IMAGE)).is_err() {
        panic!("The {APP_IMAGE} file is missing, have you ran cargo xtask build or a similar command?");
    }
    if fs::metadata(images_path.join(RECOVERY_IMAGE)).is_err() {
        panic!("The {RECOVERY_IMAGE} file is missing, have you ran cargo xtask build --recovery?");
    }
}
pub(crate) fn create_boot_image(samba_crypt_args: SambaCryptArgs) {
    check_images_exist();
    println!("Creating {BOOT_IMAGE}");
    let mut boot_image =
        fs::OpenOptions::new().write(true).read(true).truncate(true).create(true).open(BOOT_IMAGE).unwrap();

    fatfs_image::init_mbr(&mut boot_image).expect("init MBR");
    create_boot_partition(&mut boot_image, samba_crypt_args).expect("create boot partition");
    create_system_partition(&mut boot_image).expect("create system partition");
    create_user_partition(&mut boot_image).expect("create user partition");

    println!("{BOOT_IMAGE} created successfully");
}

pub fn build_charge_boot() {
    println!("Creating {BOOT_IMAGE} (charge boot)");
    let mut boot_image =
        fs::OpenOptions::new().write(true).read(true).truncate(true).create(true).open(BOOT_IMAGE).unwrap();

    fatfs_image::init_mbr(&mut boot_image).expect("init MBR");
    let fs = format_and_open(&mut boot_image, 0, BOOT_PARTITION_START_SECTOR, 0x1000, BOOT_VOLUME_NAME, true)
        .expect("error formatting partition");

    let bootloader =
        build_at91bootstrap(BootloaderBuildArgs::default(), crate::bootloader::BootloaderType::Charge);
    fs.root_dir().create_file("boot.bin").unwrap().write_all(&bootloader.bytes).unwrap();
}

/// Build the hosted simulator's FAT disk images that `os/fs` mounts. They are
/// created once and never reformatted, so user data survives across builds; only
/// the derived asset trees are refreshed.
pub fn build_hosted_disk_images() {
    println!("Building hosted disk images");
    let kernel_dir = project_root().join("xous").join("kernel");
    let user_image = kernel_dir.join("disk.dat");
    let system_image = kernel_dir.join("disk_system.dat");

    if !user_image.exists() {
        println!("- Creating user volume {}", user_image.display());
        fatfs_image::create_single_partition(
            &user_image,
            HOSTED_USER_VOLUME_SIZE,
            fatfs_image::DEFAULT_START_SECTOR,
            "USER",
        )
        .expect("create disk.dat");
    }
    if !system_image.exists() {
        println!("- Creating system volume {}", system_image.display());
        fatfs_image::create_single_partition(
            &system_image,
            HOSTED_SYSTEM_VOLUME_SIZE,
            fatfs_image::DEFAULT_START_SECTOR,
            "PRIME",
        )
        .expect("create disk_system.dat");
    }

    let hosted = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| project_root().join("target"))
        .join("hosted");

    println!("- Staging keyos/common and keyos/apps into {}", system_image.display());
    let mut image =
        fs::OpenOptions::new().read(true).write(true).open(&system_image).expect("open disk_system.dat");
    let system_fs = fatfs_image::open_partition(&mut image, 0).expect("open system partition");

    stage_system_volume(
        &system_fs,
        &SystemVolume {
            apps_src: &hosted.join("keyos").join("apps"),
            exclude_app_elf: true,
            common_out: &hosted.join("sim-seed").join("keyos").join("common"),
            app_icons_src: &hosted.join(APP_ICONS_DIR),
        },
    )
    .expect("stage system volume");
}

fn print_digest_of_cosigned_file(name: &str, path: &Path) {
    const COSIGN2_HEADER_SIZE: usize = 0x800;
    let digest: String = sha2::Sha256::digest(&fs::read(path).unwrap()[COSIGN2_HEADER_SIZE..]).encode_hex();
    println!("{name:<30} - {digest}");
}

fn print_app_hashes(apps_dir: &Path, base_dir: &Path) {
    if !apps_dir.exists() {
        return;
    }

    let mut app_dirs: Vec<_> = fs::read_dir(apps_dir).unwrap().collect::<Result<_, _>>().unwrap();
    app_dirs.sort_by_key(|entry| entry.file_name());
    for app_dir in app_dirs {
        let app_path = app_dir.path();
        if !app_path.is_dir() {
            continue;
        }

        let app_elf = app_path.join("app.elf");
        if app_elf.is_file() {
            let app_name = app_path.strip_prefix(base_dir).unwrap().display().to_string();
            print_digest_of_cosigned_file(&app_name, &app_elf);
        }
        print_app_hashes(&app_path, base_dir);
    }
}

pub(crate) fn print_hashes() {
    check_images_exist();
    println!("The SHA256 hashes of all binaries (without the cosign2 header)");
    let images_path = Builder::images_path();
    let bootloader_bytes = fs::read(images_path.join(BOOTLOADER_IMAGE)).unwrap();
    let bootloader_digest: String = sha2::Sha256::digest(&bootloader_bytes).encode_hex();
    println!("bootloader (raw plaintext)     - {bootloader_digest}");
    let on_device_hash = recorded_on_device_bootloader_hash(&images_path, &bootloader_bytes)
        .expect("read the on-device bootloader hash recorded during this build");
    let digest: String = on_device_hash.encode_hex();
    println!("bootloader (on-device)         - {digest}");
    print_digest_of_cosigned_file("app image", &images_path.join(APP_IMAGE));
    print_digest_of_cosigned_file("recovery image", &images_path.join(RECOVERY_IMAGE));
    let apps_dir_local =
        project_root().join("target").join(TARGET_TRIPLE_KEYOS).join("release").join(KEYOS_APPS_DIR);
    print_app_hashes(&apps_dir_local, &apps_dir_local);
    // The sideload archives are release artifacts too. Their signed bundle files live in the
    // per-build sideload bundle dir, named by app id; the archives re-pack deterministically
    // from them, so hashing the header-stripped elf compares across signing identities. Resolve
    // the dir the way the build wrote it, so CARGO_TARGET_DIR is honoured.
    let target_root = Builder::hardware().get_target_root();
    print_app_hashes(&target_root.join(crate::builder::SIDELOAD_BUNDLES_DIR), &target_root);
}
