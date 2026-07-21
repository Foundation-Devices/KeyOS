// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::SimpleMemoryMessage;
use xous::MemoryRange;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SendSeph(pub Vec<u8>);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Option<Vec<u8>>)]
pub struct RecvSeph(pub usize);

/// Syscall with buffer for memory-based operations.
///
/// This is a LendMut message that allows the caller to pass a buffer
/// for syscalls that need to read/write data (hash operations, key derivation, etc.).
///
/// Buffer layout depends on the syscall - the handler reads input parameters
/// from the buffer and writes output back to it, matching the Flux syscall ABI.
#[derive(Debug, server::Message)]
#[response(usize)]
pub struct SyscallBuffer {
    /// The memory buffer for input/output data.
    pub buf: MemoryRange,
    /// The syscall ID.
    pub syscall_id: u32,
    /// Additional argument (interpretation depends on syscall).
    pub arg: u32,
}

impl From<SimpleMemoryMessage> for SyscallBuffer {
    fn from(msg: SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, syscall_id: msg.arg1 as u32, arg: msg.arg2 as u32 }
    }
}

impl From<SyscallBuffer> for SimpleMemoryMessage {
    fn from(syscall: SyscallBuffer) -> Self {
        Self { buf: syscall.buf, arg1: syscall.syscall_id as usize, arg2: syscall.arg as usize }
    }
}
