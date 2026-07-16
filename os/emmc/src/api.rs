// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg(keyos)]

use server::{CheckedConn, CheckedPermissions, MessageAllowed};
use xous_ipc::Buffer;

use crate::{error::EmmcError, messages::*, BLOCK_SIZE};

#[macro_export]
macro_rules! use_api {
    () => {
        mod emmc_permissions {
            use emmc::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/emmc"]
            pub struct EmmcPermissions;
        }
        type EmmcApi = emmc::api::EmmcApi<emmc_permissions::EmmcPermissions>;
    };
}

/// Holds a connection to the eMMC server.
#[derive(Debug)]
pub struct EmmcApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
    block_buffer: Buffer<'static>,
}

impl<P: CheckedPermissions> Default for EmmcApi<P> {
    fn default() -> Self { Self { conn: Default::default(), block_buffer: Buffer::new(1) } }
}

impl<P: CheckedPermissions> EmmcApi<P> {
    /// Reads blocks of eMMC memory. The caller's thread is suspended until eMMC
    /// data transfer is finished.
    pub fn read_blocks(&mut self, block_index: u32, block_data: &mut [u8]) -> Result<(), EmmcError>
    where
        P: MessageAllowed<ReadEncryptedBlocks>,
        P: MessageAllowed<ReadBlocks>,
    {
        self.read_blocks_inner(block_index, block_data, false)
    }

    /// Reads *encrypted* blocks of eMMC memory. The caller's thread is suspended until eMMC
    /// data transfer is finished.
    pub fn read_encrypted_blocks(&mut self, block_index: u32, block_data: &mut [u8]) -> Result<(), EmmcError>
    where
        P: MessageAllowed<ReadEncryptedBlocks>,
        P: MessageAllowed<ReadBlocks>,
    {
        self.read_blocks_inner(block_index, block_data, true)
    }

    pub fn read_blocks_inner(
        &mut self,
        block_index: u32,
        block_data: &mut [u8],
        is_encrypted: bool,
    ) -> Result<(), EmmcError>
    where
        P: MessageAllowed<ReadEncryptedBlocks>,
        P: MessageAllowed<ReadBlocks>,
    {
        if block_data.len() & (BLOCK_SIZE - 1) != 0 {
            return Err(EmmcError::UnalignedBufferSize);
        }

        let block_count = block_data.len() / BLOCK_SIZE;

        let (buf, slow_path) = if block_data.as_ptr() as usize & 0xfff == 0 && block_data.len() & 0xfff == 0 {
            // Fast path: just read directly into the buffer.
            (&mut *block_data, false)
        } else {
            if self.block_buffer.len() < block_data.len() {
                self.block_buffer = xous_ipc::Buffer::new(block_data.len());
            }
            (&mut self.block_buffer as &mut [u8], true)
        };

        if is_encrypted {
            self.conn.lend_mut(ReadEncryptedBlocks {
                buf: unsafe { xous::MemoryRange::new(buf.as_ptr() as usize, buf.len()).unwrap() },
                block_index,
                block_count,
            })?;
        } else {
            self.conn.lend_mut(ReadBlocks {
                buf: unsafe { xous::MemoryRange::new(buf.as_ptr() as usize, buf.len()).unwrap() },
                block_index,
                block_count,
            })?;
        };

        if slow_path {
            block_data.copy_from_slice(&self.block_buffer[..block_data.len()])
        }

        Ok(())
    }

    /// Writes blocks of eMMC memory. The caller's thread is suspended until eMMC
    /// data transfer is finished.
    pub fn write_blocks(&mut self, block_index: u32, block_data: &[u8]) -> Result<(), EmmcError>
    where
        P: MessageAllowed<WriteEncryptedBlocks>,
        P: MessageAllowed<WriteBlocks>,
    {
        self.write_blocks_inner(block_index, block_data, false)
    }

    /// Writes *encrypted* blocks of eMMC memory. The caller's thread is suspended until eMMC
    /// data transfer is finished.
    pub fn write_encrypted_blocks(&mut self, block_index: u32, block_data: &[u8]) -> Result<(), EmmcError>
    where
        P: MessageAllowed<WriteEncryptedBlocks>,
        P: MessageAllowed<WriteBlocks>,
    {
        self.write_blocks_inner(block_index, block_data, true)
    }

    fn write_blocks_inner(
        &mut self,
        block_index: u32,
        block_data: &[u8],
        is_encrypted: bool,
    ) -> Result<(), EmmcError>
    where
        P: MessageAllowed<WriteEncryptedBlocks>,
        P: MessageAllowed<WriteBlocks>,
    {
        if block_data.len() & (BLOCK_SIZE - 1) != 0 {
            return Err(EmmcError::UnalignedBufferSize);
        }

        let buf = if block_data.as_ptr() as usize & 0xfff == 0 && block_data.len() & 0xfff == 0 {
            // Fast path: Just write directly from the buffer.
            block_data
        } else {
            if self.block_buffer.len() < block_data.len() {
                self.block_buffer = xous_ipc::Buffer::new(block_data.len());
            }
            self.block_buffer[..block_data.len()].copy_from_slice(block_data);
            &self.block_buffer as &[u8]
        };

        if is_encrypted {
            self.conn.lend_mut(WriteEncryptedBlocks {
                buf: unsafe { xous::MemoryRange::new(buf.as_ptr() as usize, buf.len()).unwrap() },
                block_index,
                block_count: block_data.len() / BLOCK_SIZE,
            })?;
        } else {
            self.conn.lend_mut(WriteBlocks {
                buf: unsafe { xous::MemoryRange::new(buf.as_ptr() as usize, buf.len()).unwrap() },
                block_index,
                block_count: block_data.len() / BLOCK_SIZE,
            })?;
        };

        Ok(())
    }

    pub fn block_count(&self) -> Result<usize, EmmcError>
    where
        P: MessageAllowed<BlockCount>,
    {
        Ok(self.conn.try_send_blocking_scalar(BlockCount)?)
    }

    /// Block until all writes enqueued before this call have hit flash.
    pub fn flush(&self) -> Result<(), EmmcError>
    where
        P: MessageAllowed<Flush>,
    {
        self.conn.try_send_blocking_scalar(Flush)?;
        Ok(())
    }
}
