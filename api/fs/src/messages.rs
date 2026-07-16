// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use num_traits::{FromPrimitive, ToPrimitive};
use server::SimpleMemoryMessage;
use xous::MemoryRange;

use crate::{
    DirEntry, DirHandle, Error, FileHandle, FileSystemEvent, Location, MappedFileInTheirSpace, Metadata,
    OpenFlags, SeekFrom,
};

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<FileHandle, Error>)]
pub struct OpenFileMessage {
    pub path: String,
    pub location: Location,
    pub flags: OpenFlags,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<DirHandle, Error>)]
pub struct OpenDirMessage {
    pub path: String,
    pub location: Location,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<DirHandle, Error>)]
pub struct CreateDirMessage {
    pub path: String,
    pub location: Location,
}

#[derive(Debug, server::Message)]
#[response(Result<(), Error>)]
pub struct CloseFile(pub FileHandle);

#[derive(Debug, server::Message)]
#[response(Result<(), Error>)]
pub struct CloseDir(pub DirHandle);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(FileSystemEvent)]
pub struct SubscribeFilesystemEvent(pub Location);

#[derive(Debug, server::Message)]
#[response(())]
pub struct FormatEncryptedVolume;

#[derive(Debug, server::Message)]
#[response(Result<(), Error>)]
pub struct Flush(pub FileHandle);

#[derive(Debug, server::Message)]
#[response(Result<(), Error>)]
pub struct FlushFs(pub Location);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<MappedFileInTheirSpace, Error>)]
pub struct MapFileMessage {
    pub path: String,
    pub location: Location,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Metadata, Error>)]
pub enum GetMetadata {
    Path { path: String, location: Location },
    Handle { handle: FileHandle },
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Option<DirEntry>, Error>)]
pub struct NextEntry(pub DirHandle);

#[derive(Debug, server::Message)]
#[response(Result<usize, Error>)]
pub struct ReadBlocks {
    pub buf: MemoryRange,
    pub block_index: u32,
    pub block_count: usize,
    pub location: Location,
}

#[derive(Debug, server::Message)]
#[response(Result<usize, Error>)]
pub struct WriteBlocks {
    pub buf: MemoryRange,
    pub block_index: u32,
    pub block_count: usize,
    pub location: Location,
}

#[derive(Debug, server::Message)]
#[response(Result<usize, Error>)]
pub struct BlockCount(pub Location);

#[derive(Debug, server::Message)]
#[response(Result<usize, Error>)]
pub struct ReadFile {
    pub buf: MemoryRange,
    pub handle: FileHandle,
    pub read_len: usize,
}

#[derive(Debug, server::Message)]
#[response(Result<usize, Error>)]
pub struct WriteFile {
    pub buf: MemoryRange,
    pub handle: FileHandle,
    pub write_len: usize,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<u64, Error>)]
pub struct SeekFile {
    pub file: FileHandle,
    pub pos: SeekFrom,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), Error>)]
pub struct Remove {
    pub path: String,
    pub location: Location,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), Error>)]
pub struct AtomicCopy {
    pub src: String,
    pub dest_dir: String,
    pub rename: Option<String>,
    pub location: Location,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), Error>)]
pub struct Rename {
    pub location: Location,
    pub from: String,
    pub to: String,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), Error>)]
pub struct SetLen {
    pub handle: FileHandle,
    pub len: u64,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), Error>)]
pub struct TruncateFile(pub FileHandle);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), Error>)]
pub struct SetMtime {
    pub handle: FileHandle,
    pub datetime: crate::DateTime,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Vec<u8>, Error>)]
pub struct AsyncRead {
    pub handle: FileHandle,
    pub read_len: usize,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<usize, Error>)]
pub struct AsyncWrite {
    pub handle: FileHandle,
    pub buffer: Vec<u8>,
}

#[derive(Debug, server::Message)]
#[response(Result<usize, Error>)]
pub struct AsyncCopyBlock {
    pub from: FileHandle,
    pub to: FileHandle,
    pub len: usize,
}

#[derive(Debug, server::Message)]
#[response(Result<(), Error>)]
pub struct MountAirlock(pub bool);

#[derive(Debug, server::Message)]
#[response(Result<(), Error>)]
pub struct FormatAirlock;

#[derive(Debug, server::Message)]
#[response(Result<(), Error>)]
pub struct FormatUsb;

macro_rules! declare_msg {
    ($name:ident) => {
        #[derive(Debug, server::Message)]
        #[response(())]
        pub struct $name;
    };
}

declare_msg!(GetUsbReadAccess);
declare_msg!(GetUsbWriteAccess);
declare_msg!(GetBootReadAccess);
declare_msg!(GetBootWriteAccess);
declare_msg!(GetUserReadAccess);
declare_msg!(GetUserWriteAccess);
declare_msg!(GetSystemReadAccess);
declare_msg!(GetSystemWriteAccess);
declare_msg!(GetSystemAppDataReadAccess);
declare_msg!(GetSystemAppDataWriteAccess);
declare_msg!(GetEncryptedRootReadAccess);
declare_msg!(GetEncryptedRootWriteAccess);
declare_msg!(GetAirlockReadAccess);
declare_msg!(GetAirlockWriteAccess);

impl From<SimpleMemoryMessage> for ReadBlocks {
    fn from(msg: SimpleMemoryMessage) -> Self {
        Self {
            buf: msg.buf,
            block_index: msg.arg1 as u32,
            block_count: (msg.arg2 & 0xFFFFFF),
            location: Location::from_usize(msg.arg2 >> 24).unwrap_or(Location::EncryptedRoot),
        }
    }
}

impl From<ReadBlocks> for SimpleMemoryMessage {
    fn from(read: ReadBlocks) -> Self {
        Self {
            buf: read.buf,
            arg1: read.block_index as usize,
            arg2: (read.block_count & 0xFFFFFF) | (read.location.to_usize().unwrap() << 24),
        }
    }
}

impl From<SimpleMemoryMessage> for WriteBlocks {
    fn from(msg: SimpleMemoryMessage) -> Self {
        Self {
            buf: msg.buf,
            block_index: msg.arg1 as u32,
            block_count: (msg.arg2 & 0xFFFFFF),
            location: Location::from_usize(msg.arg2 >> 24).unwrap_or(Location::EncryptedRoot),
        }
    }
}

impl From<WriteBlocks> for SimpleMemoryMessage {
    fn from(write: WriteBlocks) -> Self {
        Self {
            buf: write.buf,
            arg1: write.block_index as usize,
            arg2: (write.block_count & 0xFFFFFF) | (write.location.to_usize().unwrap() << 24),
        }
    }
}

impl From<SimpleMemoryMessage> for ReadFile {
    fn from(msg: SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, handle: FileHandle(msg.arg1 as u32), read_len: msg.arg2 }
    }
}

impl From<ReadFile> for SimpleMemoryMessage {
    fn from(read: ReadFile) -> Self {
        Self { buf: read.buf, arg1: read.handle.0 as usize, arg2: read.read_len }
    }
}

impl From<SimpleMemoryMessage> for WriteFile {
    fn from(msg: SimpleMemoryMessage) -> Self {
        Self { buf: msg.buf, handle: FileHandle(msg.arg1 as u32), write_len: msg.arg2 }
    }
}

impl From<WriteFile> for SimpleMemoryMessage {
    fn from(write: WriteFile) -> Self {
        Self { buf: write.buf, arg1: write.handle.0 as usize, arg2: write.write_len }
    }
}

// AsScalar/FromScalar implementations for blockingScalar messages
impl server::AsScalar<4> for AsyncCopyBlock {
    fn as_scalar(&self) -> [u32; 4] { [self.from.0, self.to.0, self.len as u32, 0] }
}

impl server::FromScalar<4> for AsyncCopyBlock {
    fn from_scalar([from, to, len, _]: [u32; 4]) -> Self {
        Self { from: FileHandle(from), to: FileHandle(to), len: len as usize }
    }
}
