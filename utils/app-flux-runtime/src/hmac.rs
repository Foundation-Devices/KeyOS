// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! HMAC context management for Flux syscall emulation.
//!
//! Provides HMAC-SHA256 and HMAC-SHA512 context management with init, update,
//! and finalize operations to support Flux SDK Ethereum app syscalls.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        LazyLock, RwLock,
    },
};

use hmac::{Hmac, Mac};
use sha2::{Sha256, Sha512};

type HmacSha256 = Hmac<Sha256>;
type HmacSha512 = Hmac<Sha512>;

/// HMAC context ID counter for unique context allocation.
static NEXT_HMAC_ID: AtomicU32 = AtomicU32::new(1);

/// Global HMAC context storage.
static HMAC_CONTEXTS: LazyLock<RwLock<HashMap<u32, HmacContext>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// HMAC context variants supporting different hash algorithms.
pub enum HmacContext {
    Sha256(HmacSha256),
    Sha512(HmacSha512),
}

impl HmacContext {
    /// Returns the output size of the HMAC in bytes.
    pub fn output_size(&self) -> usize {
        match self {
            HmacContext::Sha256(_) => 32,
            HmacContext::Sha512(_) => 64,
        }
    }
}

/// Errors that can occur during HMAC operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HmacError {
    /// The specified context ID was not found.
    ContextNotFound,
    /// Invalid key length.
    InvalidKey,
}

/// Initialize a new HMAC-SHA256 context.
///
/// # Arguments
/// * `key` - The HMAC key bytes
///
/// # Returns
/// A unique context ID that can be used for subsequent update/finalize calls.
pub fn hmac_sha256_init(key: &[u8]) -> u32 {
    let id = NEXT_HMAC_ID.fetch_add(1, Ordering::Relaxed);
    let mac = HmacSha256::new_from_slice(key).unwrap_or_else(|_| {
        // HMAC accepts any key length, so this shouldn't fail
        log::error!("Failed to create HMAC-SHA256 with key length {}", key.len());
        HmacSha256::new_from_slice(&[0u8; 32]).expect("HMAC init with default key")
    });
    let mut contexts = HMAC_CONTEXTS.write().unwrap();
    contexts.insert(id, HmacContext::Sha256(mac));
    log::debug!("Created HMAC-SHA256 context with id={}", id);
    id
}

/// Initialize a new HMAC-SHA512 context.
///
/// # Arguments
/// * `key` - The HMAC key bytes
///
/// # Returns
/// A unique context ID that can be used for subsequent update/finalize calls.
pub fn hmac_sha512_init(key: &[u8]) -> u32 {
    let id = NEXT_HMAC_ID.fetch_add(1, Ordering::Relaxed);
    let mac = HmacSha512::new_from_slice(key).unwrap_or_else(|_| {
        log::error!("Failed to create HMAC-SHA512 with key length {}", key.len());
        HmacSha512::new_from_slice(&[0u8; 64]).expect("HMAC init with default key")
    });
    let mut contexts = HMAC_CONTEXTS.write().unwrap();
    contexts.insert(id, HmacContext::Sha512(mac));
    log::debug!("Created HMAC-SHA512 context with id={}", id);
    id
}

/// Update an HMAC context with additional data.
///
/// # Arguments
/// * `ctx_id` - The context ID returned from an init function
/// * `data` - The data to process
///
/// # Returns
/// `Ok(())` on success, or an error if the context was not found.
pub fn hmac_update(ctx_id: u32, data: &[u8]) -> Result<(), HmacError> {
    let mut contexts = HMAC_CONTEXTS.write().unwrap();
    let ctx = contexts.get_mut(&ctx_id).ok_or(HmacError::ContextNotFound)?;

    match ctx {
        HmacContext::Sha256(mac) => mac.update(data),
        HmacContext::Sha512(mac) => mac.update(data),
    }

    log::debug!("Updated HMAC context {} with {} bytes", ctx_id, data.len());
    Ok(())
}

/// Finalize an HMAC context and return the MAC.
///
/// This consumes the context (removes it from storage).
///
/// # Arguments
/// * `ctx_id` - The context ID returned from an init function
///
/// # Returns
/// The MAC as a `Vec<u8>`, or an error if the context was not found.
pub fn hmac_final(ctx_id: u32) -> Result<Vec<u8>, HmacError> {
    let mut contexts = HMAC_CONTEXTS.write().unwrap();
    let ctx = contexts.remove(&ctx_id).ok_or(HmacError::ContextNotFound)?;

    let result = match ctx {
        HmacContext::Sha256(mac) => mac.finalize().into_bytes().to_vec(),
        HmacContext::Sha512(mac) => mac.finalize().into_bytes().to_vec(),
    };

    log::debug!("Finalized HMAC context {}", ctx_id);
    Ok(result)
}

/// Destroy an HMAC context, freeing its resources.
///
/// # Arguments
/// * `ctx_id` - The context ID to destroy
pub fn hmac_destroy(ctx_id: u32) -> Result<(), HmacError> {
    let mut contexts = HMAC_CONTEXTS.write().unwrap();
    if contexts.remove(&ctx_id).is_some() {
        log::debug!("Destroyed HMAC context {}", ctx_id);
        Ok(())
    } else {
        Err(HmacError::ContextNotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hmac_sha256() {
        // RFC 4231 Test Case 2
        let key = b"Jefe";
        let data = b"what do ya want for nothing?";
        let expected = [
            0x5b, 0xdc, 0xc1, 0x46, 0xbf, 0x60, 0x75, 0x4e, 0x6a, 0x04, 0x24, 0x26, 0x08, 0x95, 0x75, 0xc7,
            0x5a, 0x00, 0x3f, 0x08, 0x9d, 0x27, 0x39, 0x83, 0x9d, 0xec, 0x58, 0xb9, 0x64, 0xec, 0x38, 0x43,
        ];

        let ctx_id = hmac_sha256_init(key);
        hmac_update(ctx_id, data).unwrap();
        let result = hmac_final(ctx_id).unwrap();
        assert_eq!(result.as_slice(), &expected);
    }

    #[test]
    fn test_hmac_sha512() {
        // RFC 4231 Test Case 2
        let key = b"Jefe";
        let data = b"what do ya want for nothing?";
        let expected = [
            0x16, 0x4b, 0x7a, 0x7b, 0xfc, 0xf8, 0x19, 0xe2, 0xe3, 0x95, 0xfb, 0xe7, 0x3b, 0x56, 0xe0, 0xa3,
            0x87, 0xbd, 0x64, 0x22, 0x2e, 0x83, 0x1f, 0xd6, 0x10, 0x27, 0x0c, 0xd7, 0xea, 0x25, 0x05, 0x54,
            0x97, 0x58, 0xbf, 0x75, 0xc0, 0x5a, 0x99, 0x4a, 0x6d, 0x03, 0x4f, 0x65, 0xf8, 0xf0, 0xe6, 0xfd,
            0xca, 0xea, 0xb1, 0xa3, 0x4d, 0x4a, 0x6b, 0x4b, 0x63, 0x6e, 0x07, 0x0a, 0x38, 0xbc, 0xe7, 0x37,
        ];

        let ctx_id = hmac_sha512_init(key);
        hmac_update(ctx_id, data).unwrap();
        let result = hmac_final(ctx_id).unwrap();
        assert_eq!(result.as_slice(), &expected);
    }
}
