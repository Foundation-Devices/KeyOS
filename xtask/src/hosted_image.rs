// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Build the hosted simulator's FAT disk images.
//!
//! `os/fs` mounts these instead of formatting its own, so the build owns their
//! creation. Both volumes hold user data (the user volume entirely, the system
//! volume's settings), so they are created once and never reformatted; only the
//! system volume's derived asset trees (`keyos/common`, `keyos/apps`) are
//! refreshed in place.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::bootimage::{ADDITIONAL_ICON_SIZES, DEFAULT_ICON_SIZES};
use crate::builder::project_root;

const SYSTEM_VOLUME_SIZE: u64 = 128 * 1024 * 1024;
const USER_VOLUME_SIZE: u64 = 8 * 1024 * 1024 * 1024;

/// Create the hosted disk images if missing and refresh their bundled assets.
/// The images live in the kernel's run directory, where `os/fs` opens them.
pub fn build_hosted_disk_images() {
    println!("Building hosted disk images");
    let kernel_dir = project_root().join("xous").join("kernel");
    let user_image = kernel_dir.join("disk.dat");
    let system_image = kernel_dir.join("disk_system.dat");

    if !user_image.exists() {
        println!("- Creating user volume {}", user_image.display());
        fatfs_image::create_single_partition(
            &user_image,
            USER_VOLUME_SIZE,
            fatfs_image::DEFAULT_START_SECTOR,
            "USER",
        )
        .expect("create disk.dat");
    }
    if !system_image.exists() {
        println!("- Creating system volume {}", system_image.display());
        fatfs_image::create_single_partition(
            &system_image,
            SYSTEM_VOLUME_SIZE,
            fatfs_image::DEFAULT_START_SECTOR,
            "PRIME",
        )
        .expect("create disk_system.dat");
    }

    let hosted = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| project_root().join("target"))
        .join("hosted");
    let common_seed = hosted.join("sim-seed").join("keyos").join("common");
    render_common_assets(&common_seed);
    let host_apps = hosted.join("keyos").join("apps");

    println!("- Staging keyos/common and keyos/apps into {}", system_image.display());
    let mut image =
        fs::OpenOptions::new().read(true).write(true).open(&system_image).expect("open disk_system.dat");
    let system_fs = fatfs_image::open_partition(&mut image, 0).expect("open system partition");

    fatfs_image::remove_tree(&system_fs, "keyos/common").expect("clear keyos/common");
    fatfs_image::copy_tree_into(&system_fs, &common_seed, "keyos/common").expect("stage keyos/common");
    fatfs_image::remove_tree(&system_fs, "keyos/apps").expect("clear keyos/apps");
    if host_apps.is_dir() {
        // The simulator execs app.elf as a host binary, so it stays out of the image.
        fatfs_image::copy_tree_into_excluding(&system_fs, &host_apps, "keyos/apps", &["app.elf"])
            .expect("stage keyos/apps");
    }
}

/// Render `ui/ui` images, icons, and fonts into `out_dir` in the device's on-disk
/// format, matching the system partition's `keyos/common` layout.
fn render_common_assets(out_dir: &Path) {
    if out_dir.exists() {
        fs::remove_dir_all(out_dir).expect("clean common seed dir");
    }
    fs::create_dir_all(out_dir).expect("create common seed dir");

    let ui_dir = project_root().join("ui").join("ui");

    println!("Bundling common images");
    let timer = Instant::now();
    let mut count = 0;
    let mut last_print = timer;
    render_images(&ui_dir.join("images"), &out_dir.join("images"), &mut count, &mut last_print);
    println!("- Bundled {count} images in {:.2}s", timer.elapsed().as_secs_f32());

    println!("Bundling icons");
    render_icons(&ui_dir.join("icons"), &out_dir.join("icon_set.bin"));

    println!("Bundling fonts");
    copy_fonts(&ui_dir.join("fonts"), &out_dir.join("fonts"));
}

fn render_images(src_dir: &Path, out_dir: &Path, count: &mut usize, last_print: &mut Instant) {
    fs::create_dir_all(out_dir).expect("create images out dir");
    for entry in fs::read_dir(src_dir).expect("read images dir") {
        let path = entry.expect("image entry").path();
        if is_hidden(&path) {
            continue;
        }
        if path.is_dir() {
            render_images(&path, &out_dir.join(path.file_name().unwrap()), count, last_print);
        } else {
            let (name, data) = slint_keyos_platform_build::convert_image_to_raw(&path);
            fs::write(out_dir.join(name).with_extension("raw"), data).expect("write raw image");
            *count += 1;
            if last_print.elapsed() > Duration::from_millis(500) {
                println!("  - Converted {count} files");
                *last_print = Instant::now();
            }
        }
    }
}

fn render_icons(icons_dir: &Path, out_file: &Path) {
    let icons = fs::read_dir(icons_dir)
        .expect("read icons dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().map_or(false, |ext| ext == "svg"))
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
    fs::write(out_file, slint_keyos_platform_build::convert_icons(icons)).expect("write icon_set.bin");
}

fn copy_fonts(src_dir: &Path, out_dir: &Path) {
    fs::create_dir_all(out_dir).expect("create fonts out dir");
    for entry in fs::read_dir(src_dir).expect("read fonts dir") {
        let path = entry.expect("font entry").path();
        if !is_hidden(&path) && path.is_file() {
            fs::copy(&path, out_dir.join(path.file_name().unwrap())).expect("copy font");
        }
    }
}

/// True for dotfiles like macOS `.DS_Store`. They are not assets, and feeding
/// them in would panic the image renderer or, for fonts, the runtime loader
/// when it registers the bytes.
fn is_hidden(path: &Path) -> bool {
    path.file_name().map_or(false, |name| name.to_string_lossy().starts_with('.'))
}
