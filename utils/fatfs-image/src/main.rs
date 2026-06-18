// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Create and edit FAT32 disk images for KeyOS device and hosted simulator
//! storage. Lets the build and the SDK populate images without loop-mounting or
//! root.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use fatfs_image::{
    copy_tree_into, create_single_partition, list, open_partition, remove_tree, DEFAULT_START_SECTOR,
};

#[derive(Parser)]
#[command(about = "Create and edit FAT32 disk images", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an image with a single FAT32 partition.
    Create {
        image: PathBuf,
        /// Image size in bytes, or with a K/M/G suffix (powers of 1024).
        #[arg(long)]
        size: String,
        #[arg(long, default_value_t = DEFAULT_START_SECTOR)]
        start_sector: u32,
        #[arg(long, default_value = "PRIME")]
        label: String,
    },
    /// Copy a host directory tree into a partition, overwriting existing files.
    Cp {
        image: PathBuf,
        /// Host directory to copy from.
        src: PathBuf,
        /// Destination path inside the partition (empty for the root).
        #[arg(default_value = "")]
        dest: String,
        #[arg(long, default_value_t = 0)]
        partition: u8,
    },
    /// Remove a file or directory subtree from a partition.
    Rm {
        image: PathBuf,
        path: String,
        #[arg(long, default_value_t = 0)]
        partition: u8,
    },
    /// List the entries directly under a path in a partition.
    Ls {
        image: PathBuf,
        #[arg(default_value = "")]
        path: String,
        #[arg(long, default_value_t = 0)]
        partition: u8,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Create { image, size, start_sector, label } => {
            create_single_partition(&image, parse_size(&size)?, start_sector, &label)
        }
        Command::Cp { image, src, dest, partition } => {
            let mut file = open_image(&image)?;
            let fs = open_partition(&mut file, partition)?;
            copy_tree_into(&fs, &src, &dest)
        }
        Command::Rm { image, path, partition } => {
            let mut file = open_image(&image)?;
            let fs = open_partition(&mut file, partition)?;
            remove_tree(&fs, &path)
        }
        Command::Ls { image, path, partition } => {
            let mut file = open_image(&image)?;
            let fs = open_partition(&mut file, partition)?;
            let mut stdout = std::io::stdout().lock();
            for name in list(&fs, &path)? {
                writeln!(stdout, "{name}")?;
            }
            Ok(())
        }
    }
}

fn open_image(path: &std::path::Path) -> Result<std::fs::File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open image {}", path.display()))
}

fn parse_size(text: &str) -> Result<u64> {
    let text = text.trim();
    let (digits, multiplier) = match text.as_bytes().last() {
        Some(b'K' | b'k') => (&text[..text.len() - 1], 1024),
        Some(b'M' | b'm') => (&text[..text.len() - 1], 1024 * 1024),
        Some(b'G' | b'g') => (&text[..text.len() - 1], 1024 * 1024 * 1024),
        _ => (text, 1),
    };
    let value: u64 = digits.trim().parse().with_context(|| format!("invalid size {text:?}"))?;
    let size = value * multiplier;
    if size % fatfs_image::SECTOR_SIZE != 0 {
        bail!("size {size} is not a multiple of {} bytes", fatfs_image::SECTOR_SIZE);
    }
    Ok(size)
}
