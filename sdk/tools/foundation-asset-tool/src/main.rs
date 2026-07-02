// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const RAW_IMAGE_FILE: &str = "raw-image-file";
const RAW_IMAGE_DIR: &str = "raw-image-dir";

fn main() {
    if let Err(error) = run() {
        eprintln!("foundation-asset-tool: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let Some(command) = args.next() else {
        print_usage();
        return Ok(());
    };

    if command == "--help" || command == "-h" {
        print_usage();
        return Ok(());
    }

    if command == "--version" || command == "-V" {
        println!("foundation-asset-tool {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    match command.to_string_lossy().as_ref() {
        RAW_IMAGE_FILE => {
            let source = next_path(&mut args, "source image")?;
            let destination = next_path(&mut args, "destination file")?;
            ensure_no_extra_args(args)?;
            let image_name = write_raw_image_file(&source, &destination)?;
            println!("{image_name}");
        }
        RAW_IMAGE_DIR => {
            let source = next_path(&mut args, "source image")?;
            let destination_dir = next_path(&mut args, "destination directory")?;
            ensure_no_extra_args(args)?;
            let image_name = write_raw_image_dir(&source, &destination_dir)?;
            println!("{image_name}");
        }
        other => bail!("unknown command '{other}'"),
    }

    Ok(())
}

fn next_path(args: &mut impl Iterator<Item = OsString>, label: &str) -> Result<PathBuf> {
    args.next().map(PathBuf::from).ok_or_else(|| anyhow::anyhow!("missing {label}"))
}

fn ensure_no_extra_args(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    if let Some(extra) = args.next() {
        bail!("unexpected argument '{}'", extra.to_string_lossy());
    }
    Ok(())
}

fn write_raw_image_file(source: &Path, destination: &Path) -> Result<String> {
    let (image_name, image_data) = slint_keyos_platform_build::convert_image_to_raw(source)?;
    write_file(destination, &image_data)?;
    Ok(image_name)
}

fn write_raw_image_dir(source: &Path, destination_dir: &Path) -> Result<String> {
    let (image_name, image_data) = slint_keyos_platform_build::convert_image_to_raw(source)?;
    write_file(&destination_dir.join(format!("{image_name}.raw")), &image_data)?;
    Ok(image_name)
}

fn write_file(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create asset output directory {}", parent.display()))?;
    }

    fs::write(path, contents).with_context(|| format!("failed to write asset {}", path.display()))
}

fn print_usage() {
    println!(
        "Usage:\n  foundation-asset-tool raw-image-file <source> <destination>\n  foundation-asset-tool raw-image-dir <source> <destination-dir>"
    );
}
