// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt;

use server::{xous::MemoryRange, SimpleMemoryMessage};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CryptoError, ShamirError};
use crate::Direction;

#[derive(
    Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Zeroize, ZeroizeOnDrop,
)]
#[response(Result<usize, CryptoError>)]
pub struct AesSetup {
    pub key: Vec<u8>,
    pub mode: AesMode,
}

impl fmt::Debug for AesSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AesSetup")
            .field("key", &format_args!("<{} bytes>", self.key.len()))
            .field("mode", &self.mode)
            .finish()
    }
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Zeroize, ZeroizeOnDrop)]
pub enum AesMode {
    Ecb,
    Cbc { iv: [u8; 16] },
    Ctr { iv: [u8; 16] },
    Gcm { iv: [u8; 12] },
}

#[derive(Debug, server::Message)]
#[response(Result<usize, CryptoError>)]
pub struct AesExecute {
    pub buf: MemoryRange,
    pub transfer_id: u8,
    pub direction: Direction,
    pub offset: usize,
    pub len: usize,
}

const OFFSET_OFFSET: usize = 9;
const TRANSFER_ID_OFFSET: usize = 1;
const DECRYPT_FLAG: usize = 1;

impl From<AesExecute> for SimpleMemoryMessage {
    fn from(value: AesExecute) -> Self {
        Self {
            buf: value.buf,
            arg1: value.len,
            arg2: (value.offset << OFFSET_OFFSET)
                | ((value.transfer_id as usize) << TRANSFER_ID_OFFSET)
                | match value.direction {
                    Direction::Encrypt => 0,
                    Direction::Decrypt => DECRYPT_FLAG,
                },
        }
    }
}

impl From<SimpleMemoryMessage> for AesExecute {
    fn from(value: SimpleMemoryMessage) -> Self {
        Self {
            buf: value.buf,
            len: value.arg1,
            transfer_id: (value.arg2 >> TRANSFER_ID_OFFSET) as u8,
            direction: if value.arg2 & DECRYPT_FLAG != 0 { Direction::Decrypt } else { Direction::Encrypt },
            offset: (value.arg2 >> OFFSET_OFFSET),
        }
    }
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<usize, CryptoError>)]
pub struct AesAad {
    pub transfer_id: u8,
    pub aad: Vec<u8>,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<[u8;16], CryptoError>)]
pub struct AesGcmTag {
    pub transfer_id: u8,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), CryptoError>)]
pub struct DiskEncryptUnsafe {
    pub tweak: [u8; 16],
    pub j: usize,
    pub src: usize,
    pub dst: usize,
    pub len: usize,
    pub direction: Direction,
}

#[cfg(keyos)]
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(DiskEncryptComplete)]
#[error(CryptoError)]
pub struct SubscribeDiskEncryptComplete;

/// Fired when a previously-accepted `DiskEncryptUnsafe` finishes.
/// This event has no struct body because pre-DMA failures are reported
/// synchronously, and DMA completion cannot fail without taking down the system.
#[cfg(keyos)]
#[derive(Debug, Clone, Copy)]
pub struct DiskEncryptComplete;

#[cfg(keyos)]
impl server::AsScalar<1> for DiskEncryptComplete {
    fn as_scalar(&self) -> [u32; 1] { [0] }
}

#[cfg(keyos)]
impl server::FromScalar<1> for DiskEncryptComplete {
    fn from_scalar(_: [u32; 1]) -> Self { Self }
}

#[derive(Debug, server::Message)]
pub struct AesClear(pub u8);

pub use crate::sha2::ShaAlgo;

/// Allocate a server-side SHA context slot (or overwrite an existing one) and seed it
/// with the supplied hash state. `context_id = None` on first use; the server allocates
/// a slot and returns its id. Subsequent calls with the returned id overwrite the state.
#[derive(server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<usize, CryptoError>)]
pub struct ShaSetContext {
    /// `None` → allocate a new slot; `Some(id)` → overwrite existing slot.
    pub context_id: Option<usize>,
    pub algo: ShaAlgo,
    /// Intermediate hash state in standard digest byte order (BE per word).
    /// SHA-224/256 use the first 32 bytes; SHA-384/512 use all 64.
    pub hash_state: [u8; 64],
}

impl fmt::Debug for ShaSetContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShaSetContext")
            .field("context_id", &self.context_id)
            .field("algo", &self.algo)
            .field("hash_state", &"<redacted>")
            .finish()
    }
}

/// Feed a block-aligned chunk of data into the hardware SHA engine via DMA.
/// `buf` must be page-aligned; `length` must be a multiple of the algo's block size.
/// Data always starts at offset 0 in `buf`.
#[derive(Debug, server::Message)]
#[response(Result<usize, CryptoError>)]
pub struct ShaUpdate {
    pub context_id: usize,
    pub buf: MemoryRange,
    pub length: usize,
}

impl From<ShaUpdate> for SimpleMemoryMessage {
    fn from(value: ShaUpdate) -> Self { Self { buf: value.buf, arg1: value.context_id, arg2: value.length } }
}

impl From<SimpleMemoryMessage> for ShaUpdate {
    fn from(value: SimpleMemoryMessage) -> Self {
        Self { context_id: value.arg1, buf: value.buf, length: value.arg2 }
    }
}

/// Retrieve the current intermediate hash state for a context slot.
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<ShaContextSnapshot, CryptoError>)]
pub struct ShaGetContext {
    pub context_id: usize,
}

/// Snapshot of a server-side SHA context (returned by `ShaGetContext`).
#[derive(Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ShaContextSnapshot {
    pub algo: ShaAlgo,
    pub hash_state: [u8; 64],
}

impl fmt::Debug for ShaContextSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShaContextSnapshot")
            .field("algo", &self.algo)
            .field("hash_state", &"<redacted>")
            .finish()
    }
}

/// Release a server-side SHA context slot.
#[derive(Debug, server::Message)]
pub struct ShaDrop(pub usize);

#[derive(server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Vec<u8>, CryptoError>)]
pub struct Hmac {
    pub algo: ShaAlgo,
    pub key: Vec<u8>,
    pub data: Vec<u8>,
}

impl fmt::Debug for Hmac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hmac")
            .field("algo", &self.algo)
            .field("key", &format_args!("<{} bytes>", self.key.len()))
            .field("data", &format_args!("<{} bytes>", self.data.len()))
            .finish()
    }
}

#[derive(server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Vec<Vec<u8>>, ShamirError>)]
pub struct ShamirSplit {
    pub secret: Vec<u8>,
    pub num_shares: usize,
    pub threshold: usize,
}

impl fmt::Debug for ShamirSplit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShamirSplit")
            .field("secret", &format_args!("<{} bytes>", self.secret.len()))
            .field("num_shares", &self.num_shares)
            .field("threshold", &self.threshold)
            .finish()
    }
}

#[derive(server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Vec<u8>, ShamirError>)]
pub struct ShamirRecover {
    pub indexes: Vec<usize>,
    pub shares: Vec<Vec<u8>>,
}

impl fmt::Debug for ShamirRecover {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShamirRecover")
            .field("indexes", &self.indexes)
            .field("shares", &format_args!("<{} shares>", self.shares.len()))
            .finish()
    }
}
