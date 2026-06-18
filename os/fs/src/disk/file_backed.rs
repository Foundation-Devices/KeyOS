// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-file block device for hosted mode. The image itself is created by the
//! build (see the `fatfs-image` tool); here we only read and write its blocks.

use std::io::Read;
use std::io::Seek;
use std::io::Write;

use fs::BLOCK_SIZE;

use super::BlockDevice;

impl BlockDevice for std::fs::File {
    fn read_blocks(&mut self, block_idx: u32, block_buf: &mut [u8]) -> Result<(), std::io::Error> {
        self.seek(std::io::SeekFrom::Start(block_idx as u64 * BLOCK_SIZE as u64))?;
        self.read_exact(block_buf)
    }

    fn write_blocks(&mut self, block_idx: u32, block_buf: &[u8]) -> Result<(), std::io::Error> {
        self.seek(std::io::SeekFrom::Start(block_idx as u64 * BLOCK_SIZE as u64))?;
        self.write_all(block_buf)
    }

    fn flush_blocks(&mut self) -> Result<(), std::io::Error> { self.flush() }
}
