// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Build and edit FAT32 disk images with an MBR partition table.
//!
//! The layout matches what the device and the hosted `fs` server mount: a
//! protective MBR followed by FAT32 primary partitions. `os/fs` reads a
//! partition's start LBA straight from the MBR (entry at `446 + index*16`), so
//! any aligned start works.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use fatfs::{Dir, FatType, FileSystem, FormatVolumeOptions, FsOptions, ReadWriteSeek};
use fscommon::StreamSlice;
use mbrs::{AddrScheme, Mbr, PartInfo, PartType};

pub const SECTOR_SIZE: u64 = 512;

/// First partition starts at 4 KiB (8 sectors): clears the MBR and stays
/// aligned even on a 4K-native host. A hosted image is a plain file, so larger
/// flash-style alignment buys nothing.
pub const DEFAULT_START_SECTOR: u32 = 8;

const VOLUME_LABEL_LEN: usize = 11;

/// Filesystem of one partition, borrowing the backing file for the operation.
pub type PartitionFs<'a> = FileSystem<StreamSlice<&'a mut File>>;

/// Create `path` as a `size`-byte image holding a single FAT32 partition,
/// truncating any existing file. The partition spans from `start_sector` to the
/// end of the image.
pub fn create_single_partition(path: &Path, size: u64, start_sector: u32, label: &str) -> Result<()> {
    if (start_sector as u64) * SECTOR_SIZE >= size {
        bail!("start sector {start_sector} does not fit in a {size}-byte image");
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .create(true)
        .open(path)
        .with_context(|| format!("create image {}", path.display()))?;

    // Size by truncation; holes read back as zeroes, so the image stays sparse.
    file.set_len(size).with_context(|| format!("size image {}", path.display()))?;

    let total_sectors = (size / SECTOR_SIZE) as u32;
    let partition_sectors = total_sectors - start_sector;

    init_mbr(&mut file)?;
    set_partition_entry(&mut file, 0, start_sector, partition_sectors, false)?;
    format_partition(&mut file, start_sector, partition_sectors, label)?;
    Ok(())
}

/// Open partition `index` of an existing image for read/write.
pub fn open_partition(file: &mut File, index: u8) -> Result<PartitionFs<'_>> {
    let (start_sector, sectors) = partition_bounds(file, index)?;
    let start = start_sector as u64 * SECTOR_SIZE;
    let end = start + sectors as u64 * SECTOR_SIZE;
    let slice = StreamSlice::new(file, start, end).context("slice partition")?;
    FileSystem::new(slice, FsOptions::new()).context("open filesystem")
}

/// Merge the host directory `src` into `dest` (a slash-separated path inside the
/// partition, empty for the root), creating missing directories and overwriting
/// existing files.
pub fn copy_tree_into<T: ReadWriteSeek>(fs: &FileSystem<T>, src: &Path, dest: &str) -> Result<()> {
    copy_tree_into_excluding(fs, src, dest, &[])
}

/// Like [`copy_tree_into`], but skips any entry whose file name is in `exclude`
/// (matched at every depth).
pub fn copy_tree_into_excluding<T: ReadWriteSeek>(
    fs: &FileSystem<T>,
    src: &Path,
    dest: &str,
    exclude: &[&str],
) -> Result<()> {
    let dir = ensure_dir(&fs.root_dir(), dest)?;
    copy_tree(&dir, src, exclude)
}

/// Remove `path` (file or directory subtree) from the partition. A missing path
/// is not an error, so a subtree can be cleared unconditionally before re-injecting.
pub fn remove_tree<T: ReadWriteSeek>(fs: &FileSystem<T>, path: &str) -> Result<()> {
    let root = fs.root_dir();
    if let Ok(dir) = root.open_dir(path) {
        clear_dir(&dir)?;
    } else if root.open_file(path).is_err() {
        return Ok(());
    }
    root.remove(path).with_context(|| format!("remove {path}"))
}

/// List the names directly under `path` (empty for the root). Directory names
/// get a trailing slash.
pub fn list<T: ReadWriteSeek>(fs: &FileSystem<T>, path: &str) -> Result<Vec<String>> {
    let root = fs.root_dir();
    let dir =
        if path.is_empty() { root } else { root.open_dir(path).with_context(|| format!("open {path}"))? };
    let mut names = Vec::new();
    for entry in dir.iter() {
        let entry = entry.context("read directory entry")?;
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        names.push(if entry.is_dir() { format!("{name}/") } else { name });
    }
    names.sort();
    Ok(names)
}

fn copy_tree<T: ReadWriteSeek>(dir: &Dir<'_, T>, src: &Path, exclude: &[&str]) -> Result<()> {
    let mut entries = fs::read_dir(src)
        .with_context(|| format!("read {}", src.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let name = entry.file_name();
        let name = name.to_str().with_context(|| format!("non-UTF-8 name in {}", src.display()))?;
        // .DS_Store is macOS noise that would otherwise leak into shipped images.
        if name == ".DS_Store" || exclude.contains(&name) {
            continue;
        }

        let path = entry.path();
        if path.is_dir() {
            let sub = open_or_create_dir(dir, name)?;
            copy_tree(&sub, &path, exclude)?;
        } else {
            let mut file = dir.create_file(name).with_context(|| format!("create {name}"))?;
            // Truncate first, or a shorter replacement leaves stale tail bytes.
            file.truncate().with_context(|| format!("truncate {name}"))?;
            let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            file.write_all(&bytes).with_context(|| format!("write {name}"))?;
        }
    }
    Ok(())
}

fn clear_dir<T: ReadWriteSeek>(dir: &Dir<'_, T>) -> Result<()> {
    // Collect before removing; the iterator can't survive mutating its own directory.
    let children: Vec<(String, bool)> = dir
        .iter()
        .filter_map(|entry| entry.ok())
        .map(|entry| (entry.file_name(), entry.is_dir()))
        .filter(|(name, _)| name != "." && name != "..")
        .collect();

    for (name, is_dir) in children {
        if is_dir {
            clear_dir(&dir.open_dir(&name).with_context(|| format!("open dir {name}"))?)?;
        }
        dir.remove(&name).with_context(|| format!("remove {name}"))?;
    }
    Ok(())
}

/// Walk a slash-separated path, opening or creating each component.
fn ensure_dir<'a, T: ReadWriteSeek>(root: &Dir<'a, T>, path: &str) -> Result<Dir<'a, T>> {
    let mut dir = root.clone();
    for component in path.split('/').filter(|c| !c.is_empty()) {
        dir = open_or_create_dir(&dir, component)?;
    }
    Ok(dir)
}

fn open_or_create_dir<'a, T: ReadWriteSeek>(parent: &Dir<'a, T>, name: &str) -> Result<Dir<'a, T>> {
    match parent.open_dir(name) {
        Ok(dir) => Ok(dir),
        Err(_) => parent.create_dir(name).with_context(|| format!("create dir {name}")),
    }
}

/// Write a fresh protective MBR (no partition entries) at sector 0.
pub fn init_mbr(file: &mut File) -> Result<()> {
    file.seek(SeekFrom::Start(0)).context("seek to MBR")?;
    let buf = <[u8; 512]>::try_from(&Mbr::default()).context("encode MBR")?;
    file.write_all(&buf).context("write MBR")?;
    Ok(())
}

/// Point MBR entry `index` at a FAT32 partition spanning `sectors` from
/// `start_sector`. `bootable` sets the active flag (only the boot partition
/// needs it). The MBR must already be initialized.
pub fn set_partition_entry(
    file: &mut File,
    index: usize,
    start_sector: u32,
    sectors: u32,
    bootable: bool,
) -> Result<()> {
    file.seek(SeekFrom::Start(0)).context("seek to MBR")?;
    let mut mbr = Mbr::try_from_reader(&*file).context("MBR must be initialized first")?;
    let last_sector = start_sector + sectors - 1;
    mbr.partition_table.entries[index] = Some(
        PartInfo::try_from_lba_bounds(
            bootable,
            start_sector,
            last_sector,
            PartType::Fat32 { visible: true, scheme: AddrScheme::Lba },
        )
        .context("build partition entry")?,
    );

    file.seek(SeekFrom::Start(0)).context("seek to MBR")?;
    let buf = <[u8; 512]>::try_from(&mbr).context("encode MBR")?;
    file.write_all(&buf).context("write MBR")?;
    Ok(())
}

/// Format the `sectors`-long region starting at `start_sector` as FAT32. Does
/// not touch the MBR; set the partition entry separately.
pub fn format_partition(file: &mut File, start_sector: u32, sectors: u32, label: &str) -> Result<()> {
    let start = start_sector as u64 * SECTOR_SIZE;
    let end = start + sectors as u64 * SECTOR_SIZE;
    let slice = StreamSlice::new(file, start, end).context("slice partition")?;

    fatfs::format_volume(
        slice,
        FormatVolumeOptions::new()
            .fat_type(FatType::Fat32)
            .total_sectors(sectors)
            // 32 KiB clusters align data to flash pages on device; harmless on hosted.
            .bytes_per_cluster(64 * SECTOR_SIZE as u32)
            .volume_label(volume_label(label)),
    )
    .context("format volume")?;
    Ok(())
}

/// Read partition `index`'s start sector and length (in sectors) from the MBR.
fn partition_bounds(file: &mut File, index: u8) -> Result<(u32, u32)> {
    if index >= 4 {
        bail!("partition index {index} out of range (0..4)");
    }
    let mut mbr = [0u8; SECTOR_SIZE as usize];
    file.seek(SeekFrom::Start(0)).context("seek to MBR")?;
    file.read_exact(&mut mbr).context("read MBR")?;

    let entry = &mbr[446 + index as usize * 16..][..16];
    let start = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
    let sectors = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]);
    if sectors == 0 {
        bail!("partition {index} is empty or absent");
    }
    Ok((start, sectors))
}

/// Pad or truncate to the 11-byte FAT volume label.
fn volume_label(label: &str) -> [u8; VOLUME_LABEL_LEN] {
    let mut out = [b' '; VOLUME_LABEL_LEN];
    for (slot, byte) in out.iter_mut().zip(label.bytes()) {
        *slot = byte;
    }
    out
}
