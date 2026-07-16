// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(dead_code))]

use crate::error::EmmcError;

#[derive(Debug, server::Message)]
#[response(Result<usize, EmmcError>)]
pub struct ReadBlocks {
    pub(crate) buf: xous::MemoryRange,
    pub(crate) block_index: u32,
    pub(crate) block_count: usize,
}

impl From<server::SimpleMemoryMessage> for ReadBlocks {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, block_index: msg.arg1 as u32, block_count: msg.arg2 }
    }
}

impl From<ReadBlocks> for server::SimpleMemoryMessage {
    fn from(read: ReadBlocks) -> Self {
        Self { buf: read.buf, arg1: read.block_index as usize, arg2: read.block_count }
    }
}

#[derive(Debug, server::Message)]
#[response(Result<usize, EmmcError>)]
pub struct WriteBlocks {
    pub(crate) buf: xous::MemoryRange,
    pub(crate) block_index: u32,
    pub(crate) block_count: usize,
}

impl From<server::SimpleMemoryMessage> for WriteBlocks {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, block_index: msg.arg1 as u32, block_count: msg.arg2 }
    }
}

impl From<WriteBlocks> for server::SimpleMemoryMessage {
    fn from(write: WriteBlocks) -> Self {
        Self { buf: write.buf, arg1: write.block_index as usize, arg2: write.block_count }
    }
}

#[derive(Debug, server::Message)]
#[response(usize)]
pub struct BlockCount;

#[derive(Debug, server::Message)]
pub(crate) struct Suspend;

#[derive(Debug, server::Message)]
#[response(Result<usize, EmmcError>)]
pub struct ReadEncryptedBlocks {
    pub(crate) buf: xous::MemoryRange,
    pub(crate) block_index: u32,
    pub(crate) block_count: usize,
}

impl From<server::SimpleMemoryMessage> for ReadEncryptedBlocks {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, block_index: msg.arg1 as u32, block_count: msg.arg2 }
    }
}

impl From<ReadEncryptedBlocks> for server::SimpleMemoryMessage {
    fn from(read: ReadEncryptedBlocks) -> Self {
        Self { buf: read.buf, arg1: read.block_index as usize, arg2: read.block_count }
    }
}

#[derive(Debug, server::Message)]
#[response(Result<usize, EmmcError>)]
pub struct WriteEncryptedBlocks {
    pub(crate) buf: xous::MemoryRange,
    pub(crate) block_index: u32,
    pub(crate) block_count: usize,
}

impl From<server::SimpleMemoryMessage> for WriteEncryptedBlocks {
    fn from(msg: server::SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, block_index: msg.arg1 as u32, block_count: msg.arg2 }
    }
}

impl From<WriteEncryptedBlocks> for server::SimpleMemoryMessage {
    fn from(write: WriteEncryptedBlocks) -> Self {
        Self { buf: write.buf, arg1: write.block_index as usize, arg2: write.block_count }
    }
}

/// Barrier: blocks the caller until all writes enqueued before this message have hit flash.
#[derive(Debug, Clone, Copy, server::Message)]
#[response(())]
pub struct Flush;

/// In-process scalar sent by the SDMMC IRQ handler to the eMMC server to signal DMA completion.
#[derive(Debug, Clone, Copy, server::Message)]
pub(crate) struct SdmmcDone;
