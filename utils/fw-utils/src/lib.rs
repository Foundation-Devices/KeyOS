// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Read;

use fs::{FileSystem, Location, OpenFlags};
use server::{CheckedPermissions, MessageAllowed};
use thiserror::Error;
use xous::{DropDeallocate, MemoryRange, PAGE_SIZE};

pub mod hash;

/// Buffer size for chunked file I/O. Must stay a multiple of `PAGE_SIZE` and
/// `SHA_DMA_ALIGNMENT`, because the streaming cosign2 hasher consumes the same
/// chunks straight into the SHA DMA engine.
pub(crate) const CHUNK_SIZE_BYTES: usize = 32 * 64 * 512; // 1 mb

#[derive(Debug, Error)]
pub enum FileError {
    #[error("xous error: {0:?}")]
    XousError(xous::Error),

    #[error("fs error: {0:?}")]
    FsError(#[from] fs::Error),

    #[error("io error: {0:?}")]
    IoError(#[from] std::io::Error),
}

impl From<xous::Error> for FileError {
    fn from(value: xous::Error) -> Self { FileError::XousError(value) }
}

pub fn read_file<P>(
    fs: &FileSystem<P>,
    path: impl Into<String>,
    location: Location,
) -> Result<(DropDeallocate, usize), FileError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::GetMetadata>,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::ReadFile>,
    P: MessageAllowed<fs::messages::CloseFile>,
{
    let path_str = path.into();
    let metadata = fs.metadata(path_str.clone(), location)?;

    let mut file =
        fs.open_file(path_str.clone(), location, OpenFlags { read: true, write: false, create: false })?;
    let size_aligned =
        if metadata.size == 0 { PAGE_SIZE as u64 } else { metadata.size.next_multiple_of(PAGE_SIZE as u64) };
    let total_size = metadata.size as usize;

    let mut file_mem =
        DropDeallocate::new(xous::map_memory(None, None, size_aligned as usize, xous::MemoryFlags::W)?);

    file.read_exact(&mut file_mem.as_slice_mut()[..total_size])?;

    Ok((file_mem, total_size))
}

pub fn write_file_progress<P>(
    fs: &FileSystem<P>,
    path: impl Into<String>,
    location: Location,
    mem: &MemoryRange,
    total_size: usize,
    progress_fn: impl Fn(f32),
) -> Result<(), FileError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::GetMetadata>,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::WriteFile>,
    P: MessageAllowed<fs::messages::Flush>,
    P: MessageAllowed<fs::messages::CloseFile>,
    P: MessageAllowed<fs::messages::TruncateFile>,
{
    use std::io::Write;

    let path_str = path.into();

    let mut file =
        fs.open_file(path_str.clone(), location, OpenFlags { read: false, write: true, create: true })?;

    progress_fn(0.0);
    for (chunk_num, chunk) in mem.as_slice()[..total_size].chunks(CHUNK_SIZE_BYTES).enumerate() {
        file.write_all(chunk)?;

        let progress = (CHUNK_SIZE_BYTES as f32 * chunk_num as f32) / total_size as f32;
        progress_fn(progress);
    }

    file.truncate()?;

    progress_fn(1.0);
    Ok(())
}

pub fn read_progress<R: std::io::Read>(
    mut reader: R,
    size: usize,
    progress_fn: impl Fn(f32),
) -> Result<(DropDeallocate, usize), FileError> {
    let size_aligned = if size == 0 { PAGE_SIZE } else { size.next_multiple_of(PAGE_SIZE) };
    let total_size = size;

    let mut file_mem = DropDeallocate::new(xous::map_memory(None, None, size_aligned, xous::MemoryFlags::W)?);

    progress_fn(0.0);

    for (chunk_num, chunk) in file_mem.as_slice_mut()[..total_size].chunks_mut(CHUNK_SIZE_BYTES).enumerate() {
        reader.read_exact(chunk)?;

        let progress = (CHUNK_SIZE_BYTES as f32 * chunk_num as f32) / total_size as f32;
        progress_fn(progress);
    }

    progress_fn(1.0);
    Ok((file_mem, total_size))
}

pub fn read_file_progress<P>(
    fs: &FileSystem<P>,
    path: impl Into<String>,
    location: Location,
    progress_fn: impl Fn(f32),
) -> Result<(DropDeallocate, usize), FileError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::GetMetadata>,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::ReadFile>,
    P: MessageAllowed<fs::messages::CloseFile>,
{
    let path_str = path.into();
    let metadata = fs.metadata(path_str.clone(), location)?;

    let file =
        fs.open_file(path_str.clone(), location, OpenFlags { read: true, write: false, create: false })?;

    read_progress(file, metadata.size as usize, progress_fn)
}

pub fn copy_file_progress<P>(
    fs: &FileSystem<P>,
    path_src: impl Into<String>,
    location_src: Location,
    path_dst: impl Into<String>,
    location_dst: Location,
    progress_fn: impl Fn(f32),
) -> Result<(), FileError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::GetMetadata>,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::ReadFile>,
    P: MessageAllowed<fs::messages::WriteFile>,
    P: MessageAllowed<fs::messages::Flush>,
    P: MessageAllowed<fs::messages::CloseFile>,
    P: MessageAllowed<fs::messages::TruncateFile>,
{
    use std::io::Write;

    let path_src_str = path_src.into();
    let metadata = fs.metadata(path_src_str.clone(), location_src)?;

    let mut file_src = fs.open_file(
        path_src_str.clone(),
        location_src,
        OpenFlags { read: true, write: false, create: false },
    )?;

    let path_dst_str = path_dst.into();
    let mut file_dst =
        fs.open_file(path_dst_str, location_dst, OpenFlags { read: false, write: true, create: true })?;

    let total_size = metadata.size as usize;
    let mut buffer = vec![0u8; CHUNK_SIZE_BYTES];

    progress_fn(0.0);

    let mut bytes_copied = 0;
    while bytes_copied < total_size {
        let bytes_remaining = total_size - bytes_copied;
        let chunk_size = bytes_remaining.min(CHUNK_SIZE_BYTES);

        file_src.read_exact(&mut buffer[..chunk_size])?;
        file_dst.write_all(&buffer[..chunk_size])?;

        bytes_copied += chunk_size;
        progress_fn(bytes_copied as f32 / total_size as f32);
    }

    file_dst.truncate()?;
    progress_fn(1.0);

    Ok(())
}

/// Copies from any reader to a file without loading it entirely into memory.
/// If the destination file already exists and is larger than `total_size`,
/// it will be truncated to the new size.
pub fn stream_to_file_progress<P, R: Read>(
    fs: &FileSystem<P>,
    mut reader: R,
    total_size: usize,
    path_dst: impl Into<String>,
    location_dst: Location,
    progress_fn: impl Fn(f32),
) -> Result<(), FileError>
where
    P: CheckedPermissions,
    P: MessageAllowed<fs::messages::OpenFileMessage>,
    P: MessageAllowed<fs::messages::WriteFile>,
    P: MessageAllowed<fs::messages::Flush>,
    P: MessageAllowed<fs::messages::CloseFile>,
    P: MessageAllowed<fs::messages::TruncateFile>,
{
    use std::io::Write;

    let path_dst_str = path_dst.into();
    let mut file_dst =
        fs.open_file(path_dst_str, location_dst, OpenFlags { read: false, write: true, create: true })?;

    let mut buffer = vec![0u8; CHUNK_SIZE_BYTES];

    progress_fn(0.0);

    let mut bytes_written = 0;
    while bytes_written < total_size {
        let bytes_remaining = total_size - bytes_written;
        let chunk_size = bytes_remaining.min(CHUNK_SIZE_BYTES);

        reader.read_exact(&mut buffer[..chunk_size])?;
        file_dst.write_all(&buffer[..chunk_size])?;

        bytes_written += chunk_size;
        progress_fn(bytes_written as f32 / total_size as f32);
    }

    file_dst.truncate()?;
    progress_fn(1.0);

    Ok(())
}
