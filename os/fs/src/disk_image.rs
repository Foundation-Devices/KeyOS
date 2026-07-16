// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{Read, Seek, SeekFrom, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use fatfs::{read_fat, write_fat, FatType, FatValue, FileSystem, ReadWriteSeek};
use fs::BLOCK_SIZE;
use xous::DropDeallocate;

use crate::disk::BlockDevice;

pub struct DiskImage<U: ReadWriteSeek + 'static> {
    size: u64,
    cluster_size: u32,
    image_first_cluster: u32,
    fs: &'static FileSystem<U>,
}

impl<U: ReadWriteSeek> DiskImage<U> {
    pub fn new(fs: &'static FileSystem<U>, name: &str, size: u64) -> std::io::Result<Self> {
        assert_eq!(fs.fat_type(), FatType::Fat32);
        let cluster_size = fs.cluster_size();
        let f = fs.root_dir().create_file(name)?;
        let image_first_cluster = f.first_cluster().unwrap_or(0);
        let mut this = Self { size, cluster_size, image_first_cluster, fs };

        if image_first_cluster == 0 {
            this.create_map_file(f)?;
        }
        Ok(this)
    }

    // Creates a contiguous file on the disk. Returns the first cluster of the file
    fn create_map_file(&mut self, mut f: fatfs::File<U>) -> std::io::Result<()> {
        let mut first_cluster = 2;
        let needed_clusters = self.mapping_cluster_number();
        log::info!("Creating image file, number of mapping clusters: {needed_clusters}");
        let mut fat = self.fs.fat_slice();
        for i in 2..self.fs.total_clusters() {
            if read_fat(&mut fat, FatType::Fat32, i)? != FatValue::Free {
                first_cluster = i + 1;
            }
            let last_cluster = first_cluster + needed_clusters - 1;
            // Big enough span of free clusters found
            if i >= last_cluster {
                // Build cluster chain in FAT
                for c in first_cluster..last_cluster {
                    write_fat(&mut fat, FatType::Fat32, c, FatValue::Data(c + 1))?;
                }
                write_fat(&mut fat, FatType::Fat32, last_cluster, FatValue::EndOfChain)?;
                log::debug!("Clusters of mapping: {first_cluster:x}..={:x}", last_cluster);

                // Zero out allocated clusters
                let mut underlying = self.fs.fs_io_adapter();
                underlying.seek(SeekFrom::Start(self.fs.offset_from_cluster(first_cluster)))?;
                // Nice big chunk of well-aligned memory for optimal writing experience
                let zeros = DropDeallocate::new(
                    xous::map_memory(
                        None,
                        None,
                        self.cluster_size as usize,
                        xous::MemoryFlags::W | xous::MemoryFlags::POPULATE,
                    )
                    .unwrap(),
                );
                for _ in 0..needed_clusters {
                    underlying.write_all(zeros.as_slice())?;
                }

                f.set_first_cluster(first_cluster);
                self.image_first_cluster = first_cluster;
                return Ok(());
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Could not find enough contiguous space on device",
        ))
    }

    fn mapping_cluster_number(&self) -> u32 {
        let mapping_size = (self.size / self.cluster_size as u64) as u32 * 4;
        mapping_size.next_multiple_of(self.cluster_size) / self.cluster_size
    }

    fn mapping_offset(&self) -> u64 { self.fs.offset_from_cluster(self.image_first_cluster) }

    fn get_cluster_mapping(&self, logical_cluster: u32) -> std::io::Result<u32> {
        let mut out = [0];
        self.get_cluster_mappings(logical_cluster, &mut out)?;
        Ok(out[0])
    }

    fn get_cluster_mappings(&self, logical_cluster_start: u32, out: &mut [u32]) -> std::io::Result<()> {
        assert!((logical_cluster_start as u64 + out.len() as u64) * (self.cluster_size as u64) <= self.size);
        let entry_pos = self.mapping_offset() + logical_cluster_start as u64 * 4;
        let mut underlying = self.fs.fs_io_adapter();
        underlying.seek(SeekFrom::Start(entry_pos))?;
        underlying.read_u32_into::<LittleEndian>(out)
    }

    fn set_cluster_mapping(&mut self, logical_cluster: u32, data: u32) -> std::io::Result<()> {
        assert!(logical_cluster as u64 * (self.cluster_size as u64) < self.size);
        let entry_pos = self.mapping_offset() + logical_cluster as u64 * 4;
        let mut underlying = self.fs.fs_io_adapter();
        underlying.seek(SeekFrom::Start(entry_pos))?;
        underlying.write_u32::<LittleEndian>(data)?;
        Ok(())
    }

    fn translate_or_alloc_cluster(
        &mut self,
        logical_cluster: u32,
        alloc: bool,
    ) -> std::io::Result<Option<u32>> {
        let entry = self.get_cluster_mapping(logical_cluster)?;
        if entry > 0 {
            log::trace!("Translated cluster: {logical_cluster:x} => {entry:x}");
            return Ok(Some(entry));
        };
        if !alloc {
            return Ok(None);
        }

        let mut fat = self.fs.fat_slice();
        let new_cluster = self.fs.alloc_cluster(None, true)?;
        // Mark used clusters as bad sectors so they won't be used for other purposes.
        // While not ideal, this is the best "singular value" (not a chain) that can be used
        // and doesn't just look like a filesystem corruption (like EndOfChain would)
        write_fat(&mut fat, FatType::Fat32, new_cluster, FatValue::Bad)?;
        self.set_cluster_mapping(logical_cluster, new_cluster)?;
        log::debug!("Allocated cluster: {logical_cluster:x} => {new_cluster:x}");
        Ok(Some(new_cluster))
    }

    // Mark clusters as free if mask is zero and the cluster is allocated.
    pub fn trim_clusters(&mut self, logical_cluster_start: u32, mask: &[u32]) -> std::io::Result<u32> {
        let mut trimmed = 0;
        let mut mapping = vec![0; mask.len()];
        self.get_cluster_mappings(logical_cluster_start, &mut mapping)?;
        for i in 0..mask.len() {
            if mask[i] == 0 && mapping[i] != 0 {
                self.free_cluster(logical_cluster_start + i as u32)?;
                trimmed += 1;
            }
        }
        Ok(trimmed)
    }

    fn free_cluster(&mut self, logical_cluster: u32) -> std::io::Result<()> {
        let mapped = self.get_cluster_mapping(logical_cluster)?;
        if mapped == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "Cluster was already free"));
        }
        log::debug!("Freeing cluster: {logical_cluster:x} => {mapped:x}");
        self.set_cluster_mapping(logical_cluster, 0)?;
        self.fs.free_cluster(mapped)?;
        Ok(())
    }

    fn translate_pos(&mut self, pos: u64, alloc: bool) -> std::io::Result<(Option<u64>, usize)> {
        let logical_cluster = (pos / self.cluster_size as u64) as u32;
        let offset_in_cluster = pos & (self.cluster_size as u64 - 1);
        let translated_offset = self
            .translate_or_alloc_cluster(logical_cluster, alloc)?
            .map(|physical_cluster| self.fs.offset_from_cluster(physical_cluster) + offset_in_cluster);
        Ok((translated_offset, (self.cluster_size as u64 - offset_in_cluster) as usize))
    }
}

impl<U: ReadWriteSeek> BlockDevice for DiskImage<U> {
    fn read_blocks(&mut self, block_idx: u32, mut block_buf: &mut [u8]) -> Result<(), std::io::Error> {
        let mut pos = block_idx as u64 * BLOCK_SIZE;
        if pos + block_buf.len() as u64 > self.size {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Read overflow"));
        }
        log::trace!("Reading {} bytes @ {}", block_buf.len(), pos);
        let mut underlying = self.fs.fs_io_adapter();
        while !block_buf.is_empty() {
            let (physical_pos, max_bytes) = self.translate_pos(pos, false)?;
            let bytes = block_buf.len().min(max_bytes);
            if let Some(physical_pos) = physical_pos {
                underlying.seek(SeekFrom::Start(physical_pos))?;
                underlying.read_exact(&mut block_buf[..bytes])?;
            } else {
                block_buf[..bytes].fill(0);
            }
            block_buf = &mut block_buf[bytes..];
            pos += bytes as u64;
        }
        Ok(())
    }

    fn write_blocks(&mut self, block_idx: u32, mut block_buf: &[u8]) -> Result<(), std::io::Error> {
        let mut pos = block_idx as u64 * BLOCK_SIZE;
        if pos + block_buf.len() as u64 > self.size {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Write overflow"));
        }
        log::trace!("Writing {} bytes @ {}", block_buf.len(), pos);

        // Don't allocate blocks just to write zeroes. Unallocated ones will "read back"
        // as zeroes anyway.
        let alloc_blocks = !slice_is_just_zeroes(block_buf);

        let mut underlying = self.fs.fs_io_adapter();
        while !block_buf.is_empty() {
            let (physical_pos, max_bytes) = self.translate_pos(pos, alloc_blocks)?;
            let bytes = block_buf.len().min(max_bytes);
            if let Some(physical_pos) = physical_pos {
                underlying.seek(SeekFrom::Start(physical_pos))?;
                underlying.write_all(&block_buf[..bytes])?;
            }
            block_buf = &block_buf[bytes..];
            pos += bytes as u64;
        }
        Ok(())
    }

    fn flush_blocks(&mut self) -> Result<(), std::io::Error> { self.fs.flush_disk() }
}

fn slice_is_just_zeroes(buffer: &[u8]) -> bool {
    let (prefix, aligned, suffix) = unsafe { buffer.align_to::<u64>() };

    prefix.iter().all(|&x| x == 0) && suffix.iter().all(|&x| x == 0) && aligned.iter().all(|&x| x == 0)
}
