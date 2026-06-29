// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{CheckedConn, CheckedPermissions, MessageAllowed, ServerContext};
use xous::{map_memory, DropDeallocate, MemoryFlags};

use crate::{error::MassStorageError, messages::*, MassStorageEvent};

#[macro_export]
macro_rules! use_api {
    () => {
        mod mass_storage_permissions {
            use mass_storage_server::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/mass-storage"]
            pub struct MassStoragePermissions;
        }
        type MassStorageApi =
            mass_storage_server::api::MassStorageApi<mass_storage_permissions::MassStoragePermissions>;
    };
}

#[derive(Debug)]
pub struct MassStorageApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
    block_buffer: DropDeallocate,
}

impl<P: CheckedPermissions> Default for MassStorageApi<P> {
    fn default() -> Self { Self { conn: Default::default(), block_buffer: block_scratch(0x1000) } }
}

/// A page-aligned scratch region for transfers whose buffer isn't page-aligned.
fn block_scratch(min_len: usize) -> DropDeallocate {
    let len = min_len.next_multiple_of(0x1000).max(0x1000);
    DropDeallocate::new(map_memory(None, None, len, MemoryFlags::W).expect("mass-storage block buffer"))
}

impl<P: CheckedPermissions> MassStorageApi<P> {
    /// Reads blocks of the connected usb drive.
    pub fn read_blocks(&mut self, block_index: u32, block_data: &mut [u8]) -> Result<(), MassStorageError>
    where
        P: MessageAllowed<ReadBlocks>,
    {
        let (buf, slow_path) = if block_data.as_ptr() as usize & 0xfff == 0 && block_data.len() & 0xfff == 0 {
            // Fast path: just read directly into the buffer.
            (&mut *block_data, false)
        } else {
            if self.block_buffer.len() < block_data.len() {
                self.block_buffer = block_scratch(block_data.len());
            }
            (self.block_buffer.as_slice_mut::<u8>(), true)
        };

        self.conn.lend_mut(ReadBlocks {
            buf: unsafe { xous::MemoryRange::new(buf.as_ptr() as usize, buf.len()).unwrap() },
            block_index,
            length: block_data.len(),
        })?;

        if slow_path {
            block_data.copy_from_slice(&self.block_buffer.as_slice::<u8>()[..block_data.len()])
        }

        Ok(())
    }

    /// Writes blocks of the connected usb drive.
    pub fn write_blocks(&mut self, block_index: u32, block_data: &[u8]) -> Result<(), MassStorageError>
    where
        P: MessageAllowed<WriteBlocks>,
    {
        let buf = if block_data.as_ptr() as usize & 0xfff == 0 && block_data.len() & 0xfff == 0 {
            // Fast path: Just write directly from the buffer.
            block_data
        } else {
            if self.block_buffer.len() < block_data.len() {
                self.block_buffer = block_scratch(block_data.len());
            }
            self.block_buffer.as_slice_mut::<u8>()[..block_data.len()].copy_from_slice(block_data);
            self.block_buffer.as_slice::<u8>()
        };

        self.conn.lend_mut(WriteBlocks {
            buf: unsafe { xous::MemoryRange::new(buf.as_ptr() as usize, buf.len()).unwrap() },
            block_index,
            length: block_data.len(),
        })?;

        Ok(())
    }

    pub fn subscribe<S>(&self, listener: &mut ServerContext<S>)
    where
        S: server::Server + server::ScalarEventHandler<MassStorageEvent>,
        P: MessageAllowed<Subscribe>,
    {
        self.conn.subscribe_scalar_infallible(Subscribe, listener)
    }

    pub fn block_count(&self) -> Result<usize, MassStorageError>
    where
        P: MessageAllowed<BlockCount>,
    {
        self.conn.try_send_blocking_scalar(BlockCount)?
    }
}
