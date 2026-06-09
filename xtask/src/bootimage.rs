// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Firmware (boot) image building routines.

use std::fs::{self, File};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use fatfs::{Dir, FatType, FileSystem};
use fscommon::StreamSlice;
use hex::ToHex;
use keyos::TOTAL_FLASH_BLOCKS;
use mbrs::{AddrScheme, Mbr, PartInfo, PartType};
use sha2::Digest;

use crate::bootloader::{build_at91bootstrap, encrypt_bootloader, BootloaderBuildArgs, SambaCryptArgs};
use crate::builder::{project_root, Builder};
use crate::{
    APP_IMAGE, BOOTLOADER_IMAGE, BOOTLOADER_IMAGE_CIPHER, BOOT_ASSETS_DIR, RECOVERY_IMAGE,
    TARGET_TRIPLE_KEYOS,
};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

pub const BOOT_IMAGE: &str = "boot.img";

const BOOT_VOLUME_NAME: &[u8] = b"KEYOSBOOT  ";
const SYSTEM_VOLUME_NAME: &[u8] = b"PRIME      ";

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

const DEFAULT_ICON_SIZES: [usize; 4] = [16, 24, 32, 48];
const ADDITIONAL_ICON_SIZES: &'static [(&str, &'static [usize])] = &[
    ("alert", &[64]),
    ("decline-circle", &[64]),
    ("bitcoin", &[64]),
    ("plus", &[96]),
    ("acorn", &[96]),
    ("key", &[96]),
    ("lock", &[64, 96]),
    ("unlock", &[64, 96]),
    ("check", &[96]),
    ("close", &[96]),
    ("arrow-down", &[96]),
    ("arrow-up", &[96]),
    ("nfc-card", &[96]),
    ("device", &[128]),
    ("nfc-1-card-horiz", &[104]),
    ("nfc-1-card-vert", &[96]),
    ("info", &[64]),
    ("info2", &[96]),
    ("question-circle", &[64]),
    ("master-key", &[96]),
    ("device-nfc", &[96]),
    ("smartphone-2", &[128]),
    ("device-detailed", &[96]),
    ("laptop", &[192]),
    ("usb-cable", &[172]),
];

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

fn init_mbr(file: &mut File) -> anyhow::Result<()> {
    file.seek(std::io::SeekFrom::Start(0))?;

    let buf = <[u8; 512]>::try_from(&Mbr::default())?;
    file.write_all(&buf)?;
    file.seek(std::io::SeekFrom::Start(0))?;

    Ok(())
}

fn update_mbr(
    file: &mut File,
    is_bootable: bool,
    partition_idx: usize,
    start_sector: u32,
    last_sector: u32,
) -> anyhow::Result<Mbr> {
    file.seek(std::io::SeekFrom::Start(0))?;
    let mut mbr = Mbr::try_from_reader(&*file).context("MBR must be already initialized")?;
    file.seek(std::io::SeekFrom::Start(0))?;

    // Update the MBR
    mbr.partition_table.entries[partition_idx] = Some(PartInfo::try_from_lba_bounds(
        is_bootable,
        start_sector,
        last_sector,
        PartType::Fat32 { visible: true, scheme: AddrScheme::Lba },
    )?);

    Ok(mbr)
}

fn format_partition<'a>(
    file: &'a mut File,
    is_bootable: bool,
    partition_idx: usize,
    volume_label: &[u8],
    start_sector: u32,
    sectors: u32,
) -> anyhow::Result<FileSystem<StreamSlice<&'a mut File>>> {
    let last_sector = start_sector + sectors - 1;
    let mbr = update_mbr(file, is_bootable, partition_idx, start_sector, last_sector)?;

    let start_offset = start_sector as u64 * SECTOR_SIZE;
    let end_offset = ((start_sector + sectors) as u64 * SECTOR_SIZE) + 1;
    let partition_slice = StreamSlice::new(&*file, start_offset, end_offset)?;

    println!(
        "Formatting partition #{}, bootable: {is_bootable}, start_sector: {start_sector}, last_sector: {last_sector}",
        partition_idx
    );
    fatfs::format_volume(
        partition_slice,
        fatfs::FormatVolumeOptions::new()
            .fat_type(FatType::Fat32)
            .total_sectors(sectors)
            .bytes_per_cluster(64 * SECTOR_SIZE as u32)
            .volume_label(volume_label.try_into()?),
    )
    .context("format volume")?;

    // Overwrite the modified MBR
    file.seek(std::io::SeekFrom::Start(0))?;
    let buf = <[u8; 512]>::try_from(&mbr)?;
    file.write_all(&buf)?;

    // Open the newly formatted partition
    file.seek(std::io::SeekFrom::Start(0))?;
    let mut boot_partition = StreamSlice::new(file, start_offset, end_offset)?;
    boot_partition.seek(std::io::SeekFrom::Start(0))?;
    FileSystem::new(boot_partition, fatfs::FsOptions::new()).context("open filesystem")
}

fn create_boot_partition(file: &mut File, samba_crypt_args: SambaCryptArgs) -> anyhow::Result<()> {
    let images_path = Builder::images_path();
    let should_encrypt_bootloader = samba_crypt_args.samba_cipher_tool.is_some();
    if should_encrypt_bootloader {
        encrypt_bootloader(&images_path, samba_crypt_args);
    }

    let fs = format_partition(
        file,
        true,
        0,
        BOOT_VOLUME_NAME,
        BOOT_PARTITION_START_SECTOR,
        BOOT_PARTITION_SIZE_SECTORS,
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
        .map(|(icon, sizes)| (ui_dir_local.join("icons").join(format!("{icon}.svg")), sizes.iter().copied()));
    let fonts = read_dir(ui_dir_local.join("fonts"));

    bundle_common_files(fs.root_dir(), output_dir, images, icons, fonts)?;

    Ok(())
}

fn create_system_partition(file: &mut File) -> anyhow::Result<()> {
    let images_path = Builder::images_path();

    let fs = format_partition(
        file,
        false,
        1,
        SYSTEM_VOLUME_NAME,
        SYSTEM_PARTITION_START_SECTOR,
        SYSTEM_PARTITION_SIZE_SECTORS,
    )?;

    let keyos_dir = fs.root_dir().create_dir("keyos").context("system: creating `keyos` directory")?;
    keyos_dir.create_file(APP_IMAGE)?.write_all(&fs::read(images_path.join(APP_IMAGE))?)?;

    println!("Bundling FS apps");
    let apps_dir_keyos = keyos_dir.create_dir("apps")?;
    let apps_dir_local = project_root().join("target").join(TARGET_TRIPLE_KEYOS).join("release").join("apps");
    if apps_dir_local.exists() {
        for app_dir in fs::read_dir(apps_dir_local)? {
            let app_dir_local = app_dir?;
            let app_name = app_dir_local.file_name().into_string().unwrap();

            println!("- Bundling `{}` app", app_name);
            let app_dir_disk = apps_dir_keyos.create_dir(&app_name)?;

            const APP_FILES: &[&str] = &["app.elf", "manifest.json"];
            for app_file in APP_FILES {
                let app_file_local = app_dir_local.path().join(app_file);
                let mut app_file_disk = app_dir_disk.create_file(app_file)?;
                println!(
                    "  - Copying: {} -> /apps/{}/{}",
                    app_file_local.file_name().unwrap().to_str().unwrap(),
                    app_name,
                    app_file
                );
                app_file_disk.write_all(&fs::read(app_file_local)?)?;
            }

            let app_resources_local = app_dir_local.path().join("resources");
            if app_resources_local.is_dir() {
                println!("  - Copying app resources");
                let resources_dir_disk = app_dir_disk.create_dir("resources")?;
                copy_dir_to_fat(&app_resources_local, &resources_dir_disk)?;
            }
        }
    } else {
        println!("* no apps directory found");
    }

    let ui_dir_local = project_root().join("ui").join("ui");

    let output_dir = project_root().join("target").join(TARGET_TRIPLE_KEYOS).join("release").join("common");
    let images = read_dir(ui_dir_local.join("images"));
    let icons = read_dir(ui_dir_local.join("icons"))
        .filter(|e| e.extension().map_or(false, |f| f == "svg"))
        .map(|path| {
            let icon_name = path.file_stem().unwrap().to_string_lossy().to_string();
            let mut sizes = Vec::from(DEFAULT_ICON_SIZES);
            for (additional_name, additional_sizes) in ADDITIONAL_ICON_SIZES {
                if *additional_name == icon_name {
                    sizes.extend_from_slice(additional_sizes);
                }
            }
            (path, sizes)
        });
    let fonts = read_dir(ui_dir_local.join("fonts"));

    bundle_common_files(keyos_dir, output_dir, images, icons, fonts)?;

    Ok(())
}

fn process_image_file(
    target_dir: &Dir<'_, StreamSlice<&mut File>>,
    image_path: &Path,
    out_dir: &Path,
) -> anyhow::Result<()> {
    let (image_name, image_data) = slint_keyos_platform_build::convert_image_to_raw(image_path);
    let image_name_disk = PathBuf::from(image_name).with_extension("raw");
    let mut image_file_disk = target_dir.create_file(image_name_disk.to_str().unwrap())?;
    image_file_disk.write_all(&image_data)?;

    fs::write(out_dir.join(image_name_disk), image_data)?;

    Ok(())
}

fn process_directory(
    dir_path: &Path,
    target_dir: &Dir<'_, StreamSlice<&mut File>>,
    out_dir: &Path,
    image_count: &mut usize,
) -> anyhow::Result<()> {
    for entry in read_dir(dir_path) {
        if entry.is_dir() {
            let dir_name = entry.file_name().unwrap().to_str().unwrap();
            let sub_dir = target_dir.create_dir(dir_name)?;
            process_directory(&entry, &sub_dir, &out_dir.join(dir_name), image_count)?;
        } else if entry.is_file() {
            process_image_file(target_dir, &entry, out_dir)?;
            *image_count += 1;
        }
    }
    Ok(())
}

fn copy_dir_to_fat(dir_path: &Path, target_dir: &Dir<'_, StreamSlice<&mut File>>) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(dir_path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let entry_name = entry.file_name();
        if entry_name.to_string_lossy() == ".DS_Store" {
            continue;
        }
        let entry = entry.path();
        if entry.is_dir() {
            let dir_name = entry.file_name().unwrap().to_str().unwrap();
            let sub_dir = target_dir.create_dir(dir_name)?;
            copy_dir_to_fat(&entry, &sub_dir)?;
        } else if entry.is_file() {
            let file_name = entry.file_name().unwrap();
            let mut file_disk = target_dir.create_file(file_name.to_str().unwrap())?;
            file_disk.write_all(&fs::read(&entry)?)?;
        }
    }
    Ok(())
}

fn bundle_common_files<Images, Icons, Fonts, IconSizes>(
    root_dir: Dir<'_, StreamSlice<&mut File>>,
    output_dir: PathBuf,
    images: Images,
    icons: Icons,
    fonts: Fonts,
) -> anyhow::Result<()>
where
    Images: IntoIterator<Item = PathBuf>,
    Icons: IntoIterator<Item = (PathBuf, IconSizes)>,
    Fonts: IntoIterator<Item = PathBuf>,
    IconSizes: IntoIterator<Item = usize>,
{
    fs::remove_dir_all(&output_dir).ok();
    fs::create_dir(&output_dir).context("create output dir")?;

    let common_dir_disk = root_dir.create_dir("common")?;
    let image_dir_disk = common_dir_disk.create_dir("images")?;
    let image_out_dir = output_dir.join("images");
    fs::create_dir(&image_out_dir).context("create output dir")?;

    println!("Bundling common images");
    let timer = std::time::Instant::now();
    let mut last_print = timer;
    let mut image_count = 0;

    for image_path in images {
        if image_path.is_dir() {
            // Get the directory name and create it
            let dir_name = image_path.file_name().unwrap().to_str().unwrap();
            let sub_dir = image_dir_disk.create_dir(dir_name)?;
            let sub_out_dir = image_out_dir.join(dir_name);
            fs::create_dir(&sub_out_dir).context("create output sub directory")?;
            process_directory(&image_path, &sub_dir, &sub_out_dir, &mut image_count)?;
        } else {
            process_image_file(&image_dir_disk, &image_path, &image_out_dir)?;
            image_count += 1;
        }

        if last_print.elapsed() > std::time::Duration::from_millis(500) {
            println!("  - Converted {image_count} files");
            last_print = std::time::Instant::now();
        }
    }
    println!("- Bundled {image_count} images in {:.2}s", timer.elapsed().as_secs_f32());
    println!("Bundling icons");
    let icon_data = slint_keyos_platform_build::convert_icons(icons);
    let mut image_file_disk = common_dir_disk.create_file("icon_set.bin")?;
    image_file_disk.write_all(&icon_data)?;
    fs::write(output_dir.join("icon_set.bin"), icon_data)?;

    println!("Bundling fonts");
    let out_dir_fonts = output_dir.join("fonts");
    fs::create_dir(&out_dir_fonts)?;

    let font_dir_disk = common_dir_disk.create_dir("fonts")?;
    for font_path in fonts {
        let font_name = font_path.file_name().unwrap().to_str().unwrap();
        let mut font_file_disk = font_dir_disk.create_file(&font_name)?;
        let font_data = fs::read(&font_path)?;
        font_file_disk.write_all(&font_data)?;
        fs::write(&out_dir_fonts.join(font_name), font_data)?;
    }

    Ok(())
}

fn create_user_partition(file: &mut File) -> anyhow::Result<()> {
    let first_sector = USER_PARTITION_START_SECTOR;
    let last_sector = first_sector + USER_PARTITION_SIZE_SECTORS - 1;
    let mbr = update_mbr(file, false, 2, first_sector, last_sector)?;

    // Overwrite the modified MBR
    file.seek(std::io::SeekFrom::Start(0))?;
    let buf = <[u8; 512]>::try_from(&mbr)?;
    file.write_all(&buf)?;

    Ok(())
}

fn read_dir(path: impl AsRef<Path>) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(&path).unwrap_or_else(|e| panic!("Could not read directory {:?}: {e:?}", AsRef::as_ref(&path)))
            .map(|e| e.unwrap().path())
            // Skip hidden files such as ".DS_Store" on macOS
            .filter(|e| e.file_name().map_or(false, |f| !f.to_string_lossy().starts_with('.')))
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

    init_mbr(&mut boot_image).expect("init MBR");
    create_boot_partition(&mut boot_image, samba_crypt_args).expect("create boot partition");
    create_system_partition(&mut boot_image).expect("create system partition");
    create_user_partition(&mut boot_image).expect("create user partition");

    println!("{BOOT_IMAGE} created successfully");
}

pub fn build_charge_boot() {
    println!("Creating {BOOT_IMAGE} (charge boot)");
    let mut boot_image =
        fs::OpenOptions::new().write(true).read(true).truncate(true).create(true).open(BOOT_IMAGE).unwrap();

    init_mbr(&mut boot_image).expect("init MBR");
    let fs =
        format_partition(&mut boot_image, true, 0, BOOT_VOLUME_NAME, BOOT_PARTITION_START_SECTOR, 0x1000)
            .expect("error formatting partition");

    let bootloader_bytes =
        build_at91bootstrap(BootloaderBuildArgs::default(), crate::bootloader::BootloaderType::Charge);
    fs.root_dir().create_file("boot.bin").unwrap().write_all(&bootloader_bytes).unwrap();
}

fn print_digest_of_cosigned_file(name: &str, path: &Path) {
    const COSIGN2_HEADER_SIZE: usize = 0x800;
    let digest: String = sha2::Sha256::digest(&fs::read(path).unwrap()[COSIGN2_HEADER_SIZE..]).encode_hex();
    println!("{name:<30} - {digest}");
}

pub(crate) fn print_hashes() {
    check_images_exist();
    println!("The SHA256 hashes of all binaries (without the cosign2 header)");
    let images_path = Builder::images_path();
    let bootloader_digest: String =
        sha2::Sha256::digest(fs::read(images_path.join(BOOTLOADER_IMAGE)).unwrap()).encode_hex();
    println!("bootloader                     - {bootloader_digest}");
    print_digest_of_cosigned_file("app image", &images_path.join(APP_IMAGE));
    print_digest_of_cosigned_file("recovery image", &images_path.join(RECOVERY_IMAGE));
    let apps_dir_local = project_root().join("target").join(TARGET_TRIPLE_KEYOS).join("release").join("apps");
    if apps_dir_local.exists() {
        let mut app_dirs: Vec<_> = fs::read_dir(apps_dir_local).unwrap().collect::<Result<_, _>>().unwrap();
        app_dirs.sort_by_key(|entry| entry.file_name());
        for app_dir_local in app_dirs {
            let app_name = app_dir_local.file_name().into_string().unwrap();
            print_digest_of_cosigned_file(&app_name, &app_dir_local.path().join("app.elf"));
        }
    }
}
