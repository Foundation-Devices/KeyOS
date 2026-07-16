// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::messages::{BlockCount, ReadBlocks, WriteBlocks};

use crate::{
    disk::{DynamicDisk, PartitionInfo},
    Error, Location, Server,
};

const BLOCK_SIZE: u64 = 512;

impl server::LendMutHandler<ReadBlocks> for Server {
    fn handle(
        &mut self,
        msg: ReadBlocks,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, Error> {
        self.with_fs_disk(msg.location, |disk| {
            let PartitionInfo { start: partition_start, len_bytes } = disk.partition_info();
            if msg.block_index as u64 + msg.block_count as u64 > len_bytes / BLOCK_SIZE {
                return Err(Error::InvalidBufferLength);
            }
            disk.read_blocks(
                msg.block_index + partition_start,
                msg.buf
                    .subrange(0, msg.block_count * BLOCK_SIZE as usize)
                    .ok_or(Error::InvalidBufferLength)?
                    .as_slice_mut(),
            )?;

            Ok(msg.block_count)
        })
    }
}

impl server::LendMutHandler<WriteBlocks> for Server {
    fn handle(
        &mut self,
        msg: WriteBlocks,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, Error> {
        self.with_fs_disk(msg.location, |disk| {
            let PartitionInfo { start: partition_start, len_bytes } = disk.partition_info();
            if msg.block_index as u64 + msg.block_count as u64 > len_bytes / BLOCK_SIZE {
                return Err(Error::InvalidBufferLength);
            }
            disk.write_blocks(
                msg.block_index + partition_start,
                msg.buf
                    .subrange(0, msg.block_count * BLOCK_SIZE as usize)
                    .ok_or(Error::InvalidBufferLength)?
                    .as_slice_mut(),
            )?;

            Ok(msg.block_count)
        })
    }
}

impl server::BlockingScalarHandler<BlockCount> for Server {
    fn handle(
        &mut self,
        msg: BlockCount,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<usize, Error> {
        self.with_fs_disk(msg.0, |disk| Ok((disk.partition_info().len_bytes / BLOCK_SIZE) as usize))
    }
}

impl Server {
    fn with_fs_disk<R>(
        &mut self,
        location: Location,
        f: impl FnOnce(&mut DynamicDisk) -> Result<R, Error>,
    ) -> Result<R, Error> {
        match location {
            Location::System => self.fs_internal.fs().with_disk(f),
            #[cfg(not(feature = "recovery-os"))]
            Location::EncryptedRoot => match self.fs_user.as_ref() {
                Some(mount) => mount.fs().with_disk(f),
                None => Err(Error::NoMedia),
            },
            #[cfg(not(feature = "recovery-os"))]
            Location::Airlock => {
                if let crate::airlock::AirlockState::Unmounted(disk) = &mut self.airlock {
                    f(disk)
                } else {
                    Err(Error::FileInUse)
                }
            }
            _ => Err(Error::NoMedia),
        }
    }
}
