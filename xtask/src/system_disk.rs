// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stage the system volume's derived trees (`keyos/apps`, `keyos/common`) onto
//! an opened FAT partition, and render the `common` assets they are built from.
//!
//! The hardware boot image and the hosted disk image create their partitions
//! differently (one lives in a multi-partition flash image, the other is a
//! standalone file that keeps user data between builds), but they fill the
//! system volume the same way, so that part lives here.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fatfs::{FileSystem, ReadWriteSeek};

use crate::builder::project_root;

pub const DEFAULT_ICON_SIZES: [usize; 4] = [16, 24, 32, 48];
pub const ADDITIONAL_ICON_SIZES: &[(&str, &[usize])] = &[
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
    // Legacy mode icons
    ("legacy", &[96]),
    ("monero", &[56]),
    ("ethereum", &[56]),
    ("solana", &[56]),
    ("chain", &[56]),
];

/// What to stage onto a system volume. Only the derived trees are touched, so a
/// persistent volume keeps its settings and user data across builds.
pub struct SystemVolume<'a> {
    /// Host directory holding the staged `keyos/apps` tree. Flux child apps are
    /// nested under `gui-app-emu-flux/apps`, so a recursive copy covers them.
    pub apps_src: &'a Path,
    /// Drop `app.elf` while copying apps. The simulator execs the host binary,
    /// so the device ELF would only waste space there.
    pub exclude_app_elf: bool,
    /// Host directory to render `keyos/common` into before copying it onto the
    /// volume.
    pub common_out: &'a Path,
}

/// Refresh `keyos/apps` and `keyos/common` on `fs` from the host sources in `vol`.
pub fn stage_system_volume<T: ReadWriteSeek>(fs: &FileSystem<T>, vol: &SystemVolume) -> Result<()> {
    let ui_dir = project_root().join("ui").join("ui");
    render_common_assets(vol.common_out, read_dir(ui_dir.join("images")), ui_icons(&ui_dir.join("icons")))?;

    println!("Bundling FS apps");
    fatfs_image::remove_tree(fs, "keyos/apps").context("clear keyos/apps")?;
    if vol.apps_src.is_dir() {
        let exclude: &[&str] = if vol.exclude_app_elf { &["app.elf"] } else { &[] };
        fatfs_image::copy_tree_into_excluding(fs, vol.apps_src, "keyos/apps", exclude)
            .context("stage keyos/apps")?;
    } else {
        println!("* no apps directory found");
    }

    fatfs_image::remove_tree(fs, "keyos/common").context("clear keyos/common")?;
    fatfs_image::copy_tree_into(fs, vol.common_out, "keyos/common").context("stage keyos/common")?;

    Ok(())
}

/// Render `images` and `icons` into `out_dir` as the on-disk `common` layout
/// (`images/**.raw`, `icon_set.bin`, `fonts/*`). Fonts are always the full
/// `ui/ui/fonts` set, so they are not a parameter. `out_dir` is wiped first, so
/// it always reflects exactly the given selection.
///
/// An image entry that is a directory is rendered recursively, preserving its
/// name; the icon sizes follow `convert_icons`.
pub fn render_common_assets<Images, Icons>(out_dir: &Path, images: Images, icons: Icons) -> Result<()>
where
    Images: IntoIterator<Item = PathBuf>,
    Icons: IntoIterator<Item = (PathBuf, Vec<usize>)>,
{
    fs::remove_dir_all(out_dir).ok();
    fs::create_dir_all(out_dir).context("create common asset dir")?;

    let images_out = out_dir.join("images");
    fs::create_dir_all(&images_out).context("create images dir")?;
    println!("Bundling common images");
    let timer = Instant::now();
    let mut last_print = timer;
    let mut count = 0;
    for image in images {
        render_image_entry(&image, &images_out, &mut count, &mut last_print)?;
    }
    println!("- Bundled {count} images in {:.2}s", timer.elapsed().as_secs_f32());

    println!("Bundling icons");
    fs::write(out_dir.join("icon_set.bin"), slint_keyos_platform_build::convert_icons(icons))
        .context("write icon_set.bin")?;

    println!("Bundling fonts");
    let fonts_out = out_dir.join("fonts");
    fs::create_dir_all(&fonts_out).context("create fonts dir")?;
    let fonts_src = project_root().join("ui").join("ui").join("fonts");
    for font in read_dir(fonts_src).filter(|p| p.extension().map_or(false, |e| e == "ttf")) {
        let name = font.file_name().with_context(|| format!("font without name: {}", font.display()))?;
        fs::copy(&font, fonts_out.join(name)).with_context(|| format!("copy font {}", font.display()))?;
    }

    Ok(())
}

fn render_image_entry(
    path: &Path,
    out_dir: &Path,
    count: &mut usize,
    last_print: &mut Instant,
) -> Result<()> {
    if path.is_dir() {
        let sub =
            out_dir.join(path.file_name().with_context(|| format!("dir without name: {}", path.display()))?);
        fs::create_dir_all(&sub).context("create image subdir")?;
        for entry in read_dir(path) {
            render_image_entry(&entry, &sub, count, last_print)?;
        }
    } else {
        // A single corrupt asset shouldn't sink the whole image build; warn and move on.
        let (name, data) = match slint_keyos_platform_build::convert_image_to_raw(path) {
            Ok(image) => image,
            Err(e) => {
                eprintln!("warning: skipping image {}: {e}", path.display());
                return Ok(());
            }
        };
        let out = out_dir.join(name).with_extension("raw");
        fs::write(&out, data).with_context(|| format!("write {}", out.display()))?;
        *count += 1;
        if last_print.elapsed() > Duration::from_millis(500) {
            println!("  - Converted {count} files");
            *last_print = Instant::now();
        }
    }
    Ok(())
}

/// The `ui/ui/icons` SVGs paired with their render sizes: every icon gets
/// [`DEFAULT_ICON_SIZES`], plus any extra sizes listed in [`ADDITIONAL_ICON_SIZES`].
pub fn ui_icons(icons_dir: &Path) -> Vec<(PathBuf, Vec<usize>)> {
    read_dir(icons_dir)
        .filter(|p| p.extension().map_or(false, |e| e == "svg"))
        .map(|path| {
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            let mut sizes = Vec::from(DEFAULT_ICON_SIZES);
            for (additional_name, additional_sizes) in ADDITIONAL_ICON_SIZES {
                if *additional_name == name {
                    sizes.extend_from_slice(additional_sizes);
                }
            }
            (path, sizes)
        })
        .collect()
}

/// Directory entries as paths, skipping hidden files (e.g. macOS `.DS_Store`),
/// which are not assets and would panic the image renderer.
pub fn read_dir(path: impl AsRef<Path>) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(&path)
        .unwrap_or_else(|e| panic!("Could not read directory {:?}: {e:?}", AsRef::as_ref(&path)))
        .map(|e| e.unwrap().path())
        .filter(|e| e.file_name().map_or(false, |f| !f.to_string_lossy().starts_with('.')))
}
