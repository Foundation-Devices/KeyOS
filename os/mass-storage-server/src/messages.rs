// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::{MassStorageError, MassStorageEvent};

#[derive(Debug, server::Message)]
#[response(Result<usize, MassStorageError>)]
pub struct ReadBlocks {
    pub(crate) buf: xous::MemoryRange,
    pub(crate) block_index: u32,
    pub(crate) length: usize,
}

impl From<server::SimpleMemoryMessage> for ReadBlocks {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, block_index: msg.arg1 as u32, length: msg.arg2 }
    }
}

impl From<ReadBlocks> for server::SimpleMemoryMessage {
    fn from(read: ReadBlocks) -> Self {
        Self { buf: read.buf, arg1: read.block_index as usize, arg2: read.length }
    }
}

#[derive(Debug, server::Message)]
#[response(Result<usize, MassStorageError>)]
pub struct WriteBlocks {
    pub(crate) buf: xous::MemoryRange,
    pub(crate) block_index: u32,
    pub(crate) length: usize,
}

impl From<server::SimpleMemoryMessage> for WriteBlocks {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, block_index: msg.arg1 as u32, length: msg.arg2 }
    }
}

impl From<WriteBlocks> for server::SimpleMemoryMessage {
    fn from(write: WriteBlocks) -> Self {
        Self { buf: write.buf, arg1: write.block_index as usize, arg2: write.length }
    }
}

#[derive(Debug, server::Message)]
#[response(Result<(), MassStorageError>)]
pub struct Flush;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(MassStorageEvent)]
pub struct Subscribe;

#[derive(Debug, server::Message)]
#[response(Result<usize, MassStorageError>)]
pub struct BlockCount;
