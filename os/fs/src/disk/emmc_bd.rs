// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! `emmc` server-based `fatfs` storage adapter.

use fs::BLOCK_SIZE;

use super::BlockDevice;
use crate::EmmcApi;
/// A wrapper around `Emmc` that performs encrypted operations on a subset of blocks.
pub struct PartiallyEncryptedEmmc {
    emmc: EmmcApi,
    encrypted_block_idx: u32,
    encrypted_num_blocks: u32,
}

impl PartiallyEncryptedEmmc {
    #[allow(dead_code)]
    pub fn new(emmc: EmmcApi, encrypted_block_idx: u32, encrypted_num_blocks: u32) -> Self {
        Self { emmc, encrypted_block_idx, encrypted_num_blocks }
    }
}

impl BlockDevice for EmmcApi {
    fn read_blocks(&mut self, block_idx: u32, block_buf: &mut [u8]) -> Result<(), std::io::Error> {
        self.read_blocks(block_idx, block_buf).map_err(|_| std::io::ErrorKind::Other.into())
    }

    fn write_blocks(&mut self, block_idx: u32, block_buf: &[u8]) -> Result<(), std::io::Error> {
        self.write_blocks(block_idx, block_buf).map_err(|_| std::io::ErrorKind::Other.into())
    }

    fn flush_blocks(&mut self) -> Result<(), std::io::Error> {
        self.flush().map_err(|_| std::io::ErrorKind::Other.into())
    }
}

impl BlockDevice for PartiallyEncryptedEmmc {
    fn read_blocks(&mut self, block_idx: u32, block_buf: &mut [u8]) -> Result<(), std::io::Error> {
        let num_blocks = (block_buf.len() as u64 / BLOCK_SIZE) as u32;
        let final_block_idx =
            self.encrypted_block_idx.saturating_add(self.encrypted_num_blocks.saturating_add(num_blocks));
        let encrypted_range = self.encrypted_block_idx..final_block_idx;

        // This doesn't cover accesses across partition boundary, but that's not expected to happen normally
        let res = if encrypted_range.contains(&block_idx) {
            self.emmc.read_encrypted_blocks(block_idx, block_buf)
        } else {
            self.emmc.read_blocks(block_idx, block_buf)
        };

        res.map_err(|e| {
            log::error!("Error reading encrypted blocks: {:?}", e);
            std::io::ErrorKind::Other.into()
        })
    }

    fn write_blocks(&mut self, block_idx: u32, block_buf: &[u8]) -> Result<(), std::io::Error> {
        let num_blocks = (block_buf.len() as u64 / BLOCK_SIZE) as u32;
        let final_block_idx =
            self.encrypted_block_idx.saturating_add(self.encrypted_num_blocks.saturating_add(num_blocks));
        let encrypted_range = self.encrypted_block_idx..final_block_idx;

        // This doesn't cover accesses across partition boundary, but that's not expected to happen normally
        let res = if encrypted_range.contains(&block_idx) {
            self.emmc.write_encrypted_blocks(block_idx, block_buf)
        } else {
            self.emmc.write_blocks(block_idx, block_buf)
        };

        res.map_err(|e| {
            log::error!("Error writing encrypted blocks: {:?}", e);
            std::io::ErrorKind::Other.into()
        })
    }

    fn flush_blocks(&mut self) -> Result<(), std::io::Error> {
        self.emmc.flush().map_err(|_| std::io::ErrorKind::Other.into())
    }
}
