// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hash context management for Flux syscall emulation.
//!
//! Provides Keccak-256, SHA-256, and SHA-512 hash context management with init,
//! update, and finalize operations to support Flux SDK Ethereum app syscalls.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        LazyLock, RwLock,
    },
};

use sha2::{Digest as Sha2Digest, Sha256, Sha512};
use sha3::Keccak256;

/// Flux SDK hash algorithm identifiers.
#[allow(unused)]
pub mod algo {
    pub const CX_SHA256: u8 = 0x01;
    pub const CX_SHA224: u8 = 0x02;
    pub const CX_SHA384: u8 = 0x03;
    pub const CX_SHA512: u8 = 0x04;
    pub const CX_KECCAK: u8 = 0x12;
    pub const CX_SHA3: u8 = 0x13;
    pub const CX_RIPEMD160: u8 = 0x14;
    pub const CX_BLAKE2B: u8 = 0x15;
}

/// Flux SDK hash operation flags.
#[allow(unused)]
pub mod flags {
    pub const CX_LAST: u32 = 0x01;
    pub const CX_NO_REINIT: u32 = 0x10;
}

/// Hash output sizes in bytes.
pub mod sizes {
    pub const SHA256_SIZE: usize = 32;
    pub const SHA512_SIZE: usize = 64;
    pub const KECCAK256_SIZE: usize = 32;
}

/// Hash context ID counter for unique context allocation.
static NEXT_CONTEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Global hash context storage.
static HASH_CONTEXTS: LazyLock<RwLock<HashMap<u32, HashContext>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Hash context variants supporting different hash algorithms.
pub enum HashContext {
    Keccak256(Keccak256),
    Sha256(Sha256),
    Sha512(Sha512),
}

impl HashContext {
    /// Returns the output size of the hash algorithm in bytes.
    pub fn output_size(&self) -> usize {
        match self {
            HashContext::Keccak256(_) => sizes::KECCAK256_SIZE,
            HashContext::Sha256(_) => sizes::SHA256_SIZE,
            HashContext::Sha512(_) => sizes::SHA512_SIZE,
        }
    }

    /// Resets the hash context to its initial state.
    pub fn reset(&mut self) {
        match self {
            HashContext::Keccak256(ctx) => *ctx = Keccak256::new(),
            HashContext::Sha256(ctx) => *ctx = Sha256::new(),
            HashContext::Sha512(ctx) => *ctx = Sha512::new(),
        }
    }
}

/// Errors that can occur during hash operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashError {
    /// The specified context ID was not found.
    ContextNotFound,
    /// Invalid hash algorithm specified.
    InvalidAlgorithm,
    /// Output buffer too small.
    BufferTooSmall,
}

/// Initialize a new Keccak-256 hash context.
///
/// Returns a unique context ID that can be used for subsequent update/finalize calls.
pub fn keccak_init() -> u32 {
    let id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut contexts = HASH_CONTEXTS.write().unwrap();
    contexts.insert(id, HashContext::Keccak256(Keccak256::new()));
    log::debug!("Created Keccak-256 context with id={}", id);
    id
}

/// Initialize a new SHA-256 hash context.
///
/// Returns a unique context ID that can be used for subsequent update/finalize calls.
pub fn sha256_init() -> u32 {
    let id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut contexts = HASH_CONTEXTS.write().unwrap();
    contexts.insert(id, HashContext::Sha256(Sha256::new()));
    log::debug!("Created SHA-256 context with id={}", id);
    id
}

/// Initialize a new SHA-512 hash context.
///
/// Returns a unique context ID that can be used for subsequent update/finalize calls.
pub fn sha512_init() -> u32 {
    let id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut contexts = HASH_CONTEXTS.write().unwrap();
    contexts.insert(id, HashContext::Sha512(Sha512::new()));
    log::debug!("Created SHA-512 context with id={}", id);
    id
}

/// Initialize a hash context based on algorithm identifier.
///
/// # Arguments
/// * `algorithm` - The Flux SDK hash algorithm identifier (see `algo` module)
///
/// # Returns
/// A unique context ID or 0 on error.
pub fn hash_init(algorithm: u8) -> u32 {
    match algorithm {
        algo::CX_SHA256 => sha256_init(),
        algo::CX_SHA512 => sha512_init(),
        algo::CX_KECCAK => keccak_init(),
        _ => {
            log::warn!("Unsupported hash algorithm: 0x{:02x}", algorithm);
            0
        }
    }
}

/// Update a hash context with additional data.
///
/// # Arguments
/// * `ctx_id` - The context ID returned from an init function
/// * `data` - The data to hash
///
/// # Returns
/// `Ok(())` on success, or an error if the context was not found.
pub fn hash_update(ctx_id: u32, data: &[u8]) -> Result<(), HashError> {
    let mut contexts = HASH_CONTEXTS.write().unwrap();
    let ctx = contexts.get_mut(&ctx_id).ok_or(HashError::ContextNotFound)?;

    match ctx {
        HashContext::Keccak256(hasher) => hasher.update(data),
        HashContext::Sha256(hasher) => hasher.update(data),
        HashContext::Sha512(hasher) => hasher.update(data),
    }

    log::debug!("Updated hash context {} with {} bytes", ctx_id, data.len());
    Ok(())
}

/// Finalize a hash context and return the digest.
///
/// If `CX_NO_REINIT` is set, this consumes the context and removes it from
/// storage. Otherwise, the context is automatically re-initialized for reuse.
///
/// # Arguments
/// * `ctx_id` - The context ID returned from an init function
/// * `flags` - Operation flags (CX_LAST, CX_NO_REINIT)
///
/// # Returns
/// The hash digest as a `Vec<u8>`, or an error if the context was not found.
pub fn hash_final(ctx_id: u32, flags: u32) -> Result<Vec<u8>, HashError> {
    let mut contexts = HASH_CONTEXTS.write().unwrap();
    let no_reinit = flags & flags::CX_NO_REINIT != 0;

    let result = {
        let ctx = contexts.get_mut(&ctx_id).ok_or(HashError::ContextNotFound)?;

        match ctx {
            HashContext::Keccak256(hasher) => {
                let digest = hasher.clone().finalize();
                if !no_reinit {
                    *hasher = Keccak256::new();
                }
                digest.to_vec()
            }
            HashContext::Sha256(hasher) => {
                let digest = hasher.clone().finalize();
                if !no_reinit {
                    *hasher = Sha256::new();
                }
                digest.to_vec()
            }
            HashContext::Sha512(hasher) => {
                let digest = hasher.clone().finalize();
                if !no_reinit {
                    *hasher = Sha512::new();
                }
                digest.to_vec()
            }
        }
    };
    if no_reinit {
        contexts.remove(&ctx_id);
    }

    log::debug!("Finalized hash context {} (flags=0x{:x})", ctx_id, flags);
    Ok(result)
}

/// Perform a one-shot SHA-256 hash of the input data.
///
/// # Arguments
/// * `data` - The data to hash
///
/// # Returns
/// The 32-byte SHA-256 digest.
pub fn sha256_oneshot(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Perform a one-shot SHA-512 hash of the input data.
///
/// # Arguments
/// * `data` - The data to hash
///
/// # Returns
/// The 64-byte SHA-512 digest.
pub fn sha512_oneshot(data: &[u8]) -> [u8; 64] {
    let mut hasher = Sha512::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Perform a one-shot Keccak-256 hash of the input data.
///
/// # Arguments
/// * `data` - The data to hash
///
/// # Returns
/// The 32-byte Keccak-256 digest.
pub fn keccak256_oneshot(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Destroy a hash context, freeing its resources.
///
/// # Arguments
/// * `ctx_id` - The context ID to destroy
///
/// # Returns
/// `Ok(())` on success, or an error if the context was not found.
pub fn hash_destroy(ctx_id: u32) -> Result<(), HashError> {
    let mut contexts = HASH_CONTEXTS.write().unwrap();
    if contexts.remove(&ctx_id).is_some() {
        log::debug!("Destroyed hash context {}", ctx_id);
        Ok(())
    } else {
        Err(HashError::ContextNotFound)
    }
}

/// Get information about a hash context.
///
/// # Arguments
/// * `ctx_id` - The context ID to query
///
/// # Returns
/// A tuple of (algorithm, output_size) or an error if not found.
pub fn hash_info(ctx_id: u32) -> Result<(u8, usize), HashError> {
    let contexts = HASH_CONTEXTS.read().unwrap();
    let ctx = contexts.get(&ctx_id).ok_or(HashError::ContextNotFound)?;

    let (algo, size) = match ctx {
        HashContext::Keccak256(_) => (algo::CX_KECCAK, sizes::KECCAK256_SIZE),
        HashContext::Sha256(_) => (algo::CX_SHA256, sizes::SHA256_SIZE),
        HashContext::Sha512(_) => (algo::CX_SHA512, sizes::SHA512_SIZE),
    };

    Ok((algo, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_oneshot() {
        // Test vector: SHA-256("abc")
        let expected = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
            0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(sha256_oneshot(b"abc"), expected);
    }

    #[test]
    fn test_keccak256_oneshot() {
        // Test vector: Keccak-256("") - empty string
        let expected = [
            0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
            0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
        ];
        assert_eq!(keccak256_oneshot(b""), expected);
    }

    #[test]
    fn test_incremental_sha256() {
        let ctx_id = sha256_init();
        hash_update(ctx_id, b"abc").unwrap();
        let result = hash_final(ctx_id, flags::CX_LAST).unwrap();

        let expected = sha256_oneshot(b"abc");
        assert_eq!(result.as_slice(), &expected);
    }

    #[test]
    fn test_sha512_oneshot() {
        // Test vector: SHA-512("abc")
        let expected = [
            0xdd, 0xaf, 0x35, 0xa1, 0x93, 0x61, 0x7a, 0xba, 0xcc, 0x41, 0x73, 0x49, 0xae, 0x20, 0x41, 0x31,
            0x12, 0xe6, 0xfa, 0x4e, 0x89, 0xa9, 0x7e, 0xa2, 0x0a, 0x9e, 0xee, 0xe6, 0x4b, 0x55, 0xd3, 0x9a,
            0x21, 0x92, 0x99, 0x2a, 0x27, 0x4f, 0xc1, 0xa8, 0x36, 0xba, 0x3c, 0x23, 0xa3, 0xfe, 0xeb, 0xbd,
            0x45, 0x4d, 0x44, 0x23, 0x64, 0x3c, 0xe8, 0x0e, 0x2a, 0x9a, 0xc9, 0x4f, 0xa5, 0x4c, 0xa4, 0x9f,
        ];
        assert_eq!(sha512_oneshot(b"abc"), expected);
    }

    #[test]
    fn test_incremental_sha512() {
        let ctx_id = sha512_init();
        hash_update(ctx_id, b"abc").unwrap();
        let result = hash_final(ctx_id, flags::CX_LAST).unwrap();

        let expected = sha512_oneshot(b"abc");
        assert_eq!(result.as_slice(), &expected);
    }

    #[test]
    fn test_incremental_keccak() {
        let ctx_id = keccak_init();
        hash_update(ctx_id, b"hello").unwrap();
        hash_update(ctx_id, b" world").unwrap();
        let result = hash_final(ctx_id, flags::CX_LAST).unwrap();

        let expected = keccak256_oneshot(b"hello world");
        assert_eq!(result.as_slice(), &expected);
    }
}
