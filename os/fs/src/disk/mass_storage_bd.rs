// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! `mass-storage` server-based `fatfs` storage adapter.

use super::BlockDevice;
use crate::MassStorageApi;

impl BlockDevice for MassStorageApi {
    fn read_blocks(&mut self, block_idx: u32, block_buf: &mut [u8]) -> Result<(), std::io::Error> {
        self.read_blocks(block_idx, block_buf).map_err(std::io::Error::other)
    }

    fn write_blocks(&mut self, block_idx: u32, block_buf: &[u8]) -> Result<(), std::io::Error> {
        self.write_blocks(block_idx, block_buf).map_err(std::io::Error::other)
    }

    fn flush_blocks(&mut self) -> Result<(), std::io::Error> { self.flush().map_err(std::io::Error::other) }
}
