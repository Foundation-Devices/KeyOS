// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Server for accessing eMMC storage, or a dummy file in hosted mode.

#[cfg(keyos)]
mod emmc_bd;
#[cfg(keyos)]
pub use emmc_bd::PartiallyEncryptedEmmc;

#[cfg(not(keyos))]
mod file_backed;
#[cfg(keyos)]
mod mass_storage_bd;

use enum_dispatch::enum_dispatch;
use fs::BLOCK_SIZE;

// We try to keep this at a minimum to not hold too much stuff in memory, but
// common FAT32 write should not thrash the cache.
const BLOCK_CACHE_SIZE: usize = 8;

use std::{
    io::{ErrorKind, Read, Seek, SeekFrom, Write},
    num::NonZeroUsize,
};

#[cfg(not(feature = "recovery-os"))]
use crate::disk_image::DiskImage;
#[cfg(keyos)]
use crate::{EmmcApi, MassStorageApi};

#[derive(Debug, Clone, Copy)]
pub struct PartitionInfo {
    pub start: u32,
    pub len_bytes: u64,
}

#[enum_dispatch]
pub trait BlockDevice {
    fn read_blocks(&mut self, block_idx: u32, block_buf: &mut [u8]) -> Result<(), std::io::Error>;
    fn write_blocks(&mut self, block_idx: u32, block_buf: &[u8]) -> Result<(), std::io::Error>;
    // Named like this to avoid clash with io::Write::flush
    fn flush_blocks(&mut self) -> Result<(), std::io::Error>;
}

#[derive(Debug)]
pub struct Disk<BD: BlockDevice> {
    pub block_device: BD,
    pos: u64,
    partition_info: PartitionInfo,
    block_cache: lru::LruCache<u32, CachedBlock>,
}

#[derive(Debug)]
struct CachedBlock {
    data: [u8; BLOCK_SIZE as usize],
    dirty: bool,
}

impl<BD: BlockDevice> Disk<BD> {
    pub fn new(mut block_device: BD, partition_index: u8) -> Self {
        assert!(partition_index < 4);

        let mut block = DataBlock([0; BLOCK_SIZE as usize]);
        block_device.read_blocks(0, &mut block.0).expect("Could not read partition info block");

        let start = partition_index as usize * 16;
        let partition_info = &block.0[446..][start..(start + 16)];
        let lba_start = u32::from_le_bytes([
            partition_info[8],
            partition_info[9],
            partition_info[10],
            partition_info[11],
        ]);

        let num_blocks = u32::from_le_bytes([
            partition_info[12],
            partition_info[13],
            partition_info[14],
            partition_info[15],
        ]);

        Self::new_with_partition_info(
            block_device,
            PartitionInfo { start: lba_start, len_bytes: num_blocks as u64 * BLOCK_SIZE },
        )
    }

    pub fn new_with_partition_info(block_device: BD, partition_info: PartitionInfo) -> Self {
        Self {
            block_device,
            pos: 0,
            partition_info,
            block_cache: lru::LruCache::new(NonZeroUsize::new(BLOCK_CACHE_SIZE).unwrap()),
        }
    }

    pub fn partition_info(&self) -> PartitionInfo { self.partition_info }

    fn cached_block(&mut self, block_idx: u32, do_read: bool) -> Result<&mut CachedBlock, std::io::Error> {
        if !self.block_cache.contains(&block_idx) {
            let mut cached_block = CachedBlock { data: [0; BLOCK_SIZE as usize], dirty: false };
            if do_read {
                self.block_device.read_blocks(block_idx, &mut cached_block.data)?;
            }
            if let Some((prev_idx, prev_block)) = self.block_cache.push(block_idx, cached_block) {
                if prev_block.dirty {
                    self.block_device.write_blocks(prev_idx, &prev_block.data)?;
                }
            };
        }
        Ok(self.block_cache.get_mut(&block_idx).unwrap())
    }

    pub fn read_blocks(&mut self, block_idx: u32, buf: &mut [u8]) -> Result<(), std::io::Error> {
        // Write out all cached blocks in this range in case we need to re-read them
        // Shouldn't happen often.
        for cache_idx in block_idx..(block_idx + (buf.len() as u64 / BLOCK_SIZE) as u32) {
            if let Some(cached_block) = self.block_cache.get_mut(&cache_idx) {
                if cached_block.dirty {
                    self.block_device.write_blocks(cache_idx, &cached_block.data)?;
                    cached_block.dirty = false;
                }
            }
        }
        self.block_device.read_blocks(block_idx, buf)
    }

    pub fn write_blocks(&mut self, block_idx: u32, buf: &[u8]) -> Result<(), std::io::Error> {
        self.block_device.write_blocks(block_idx, buf)?;

        // Drop all cached blocks corresponding to this range. We didn't
        // need to write them, as they would have been overwritten, and we
        // most probably don't need to keep them in cache either, as
        // these bulk-writes are rarely re-read.
        for drop_idx in block_idx..(block_idx + (buf.len() as u64 / BLOCK_SIZE) as u32) {
            self.block_cache.pop(&drop_idx);
        }
        Ok(())
    }
}

impl<BD: BlockDevice> Seek for Disk<BD> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, std::io::Error> {
        let new_pos = match pos {
            SeekFrom::Start(offset) => Ok(offset),
            SeekFrom::End(offset) => {
                let end = self.partition_info.len_bytes;
                u64::checked_add_signed(end, offset).ok_or(io_error())
            }
            SeekFrom::Current(offset) => u64::checked_add_signed(self.pos, offset).ok_or(io_error()),
        }?;

        self.pos = new_pos;
        Ok(self.pos)
    }
}

impl<BD: BlockDevice> Read for Disk<BD> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let PartitionInfo { start, len_bytes } = self.partition_info;

        let pos = self.pos;
        if pos >= len_bytes {
            return Ok(0);
        }

        let remaining = usize::try_from(len_bytes - pos).unwrap_or(usize::MAX);
        let buf_len = buf.len().min(remaining);

        let offset = (pos % BLOCK_SIZE) as usize;
        let block_idx = start + (pos / BLOCK_SIZE) as u32;
        let len;
        if offset == 0 && buf_len >= BLOCK_SIZE as usize {
            len = buf_len & !(BLOCK_SIZE as usize - 1);
            self.read_blocks(block_idx, &mut buf[0..len])?;
        } else {
            len = buf_len.min(BLOCK_SIZE as usize - offset);
            let block = self.cached_block(block_idx, true)?;
            buf[0..len].copy_from_slice(&block.data[offset..(offset + len)]);
        }
        self.pos += len as u64;

        Ok(len)
    }
}

impl<BD: BlockDevice> Write for Disk<BD> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        let PartitionInfo { start, len_bytes } = self.partition_info;
        let pos = self.pos;

        if pos >= len_bytes {
            return Ok(0);
        }

        let remaining = usize::try_from(len_bytes - pos).unwrap_or(usize::MAX);
        let buf_len = buf.len().min(remaining);

        let offset = (pos % BLOCK_SIZE) as usize;
        let block_idx = start + (pos / BLOCK_SIZE) as u32;
        let len;
        if offset == 0 && buf_len >= BLOCK_SIZE as usize {
            // Fast path
            len = buf_len & !(BLOCK_SIZE as usize - 1);
            self.write_blocks(block_idx, &buf[0..len])?;
        } else {
            len = buf_len.min(BLOCK_SIZE as usize - offset);

            let block = self.cached_block(block_idx, offset != 0 || len != BLOCK_SIZE as usize)?;
            block.data[offset..(offset + len)].copy_from_slice(&buf[0..len]);
            block.dirty = true;
        }

        self.pos += len as u64;

        Ok(len)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        for (block_idx, cached_block) in &mut self.block_cache {
            if cached_block.dirty {
                self.block_device.write_blocks(*block_idx, &cached_block.data)?;
                cached_block.dirty = false;
            }
        }
        self.block_device.flush_blocks()
    }
}

impl<BD: BlockDevice> Drop for Disk<BD> {
    fn drop(&mut self) {
        if let Err(e) = self.flush() {
            log::error!("Failed to flush disk: {:?}", e);
        }
    }
}

pub type DynamicDisk = Disk<DynamicDiskBlockDevice>;

#[enum_dispatch(BlockDevice)]
pub enum DynamicDiskBlockDevice {
    #[cfg(keyos)]
    Emmc(EmmcApi),
    #[cfg(keyos)]
    EncryptedEmmc(PartiallyEncryptedEmmc),
    #[cfg(keyos)]
    MassStorage(MassStorageApi),
    #[cfg(not(feature = "recovery-os"))]
    DiskImage(DiskImage<Disk<DynamicDiskBlockDevice>>),
    #[cfg(not(keyos))]
    File(std::fs::File),
}

#[derive(Debug, Clone)]
struct DataBlock(pub [u8; BLOCK_SIZE as usize]);

fn io_error() -> std::io::Error { std::io::Error::new(ErrorKind::InvalidData, "I/O error") }
