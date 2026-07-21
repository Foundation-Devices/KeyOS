// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Rust implementations of Flux SDK crypto functions.
//!
//! These `#[no_mangle]` functions override the weak symbols from the SDK's
//! `cx_stubs.S` trampoline, providing in-process crypto using Rust libraries
//! instead of routing through the unreachable `SHARED_TRAMPOLINE_ADDR`.

// Allow unused constants and functions that are part of the SDK API surface.
#![allow(dead_code, non_upper_case_globals)]

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar as DalekScalar;
use gui_app_emu_flux::{keys, syscall_id};
use num_bigint::BigUint;
use num_traits::{One, Zero};

use crate::runtime::syscall_buffer;

// --- SDK cx_curve_t values (from ox_ec.h) ---
const CX_CURVE_SECP256K1: u32 = 0x21;
const CX_CURVE_ED25519: u32 = 0x71;
const CX_CURVE_Curve25519: u32 = 0x72;

// --- SDK ECDSA flags ---
const CX_RND_RFC6979: u32 = 2 << 9;
const CX_ECCINFO_PARITY_ODD: u32 = 1;
const CX_ECCINFO_xGTn: u32 = 2;

// --- Subtraction carry ---
const CX_CARRY: u32 = 0xFFFFFF21;

// --- AES mode flags (from lcx_common.h) ---
const CX_AES_ENCRYPT: u32 = 2 << 1;
const CX_AES_CHAIN_CBC: u32 = 1 << 6;

struct PtrMap {
    name: &'static str,
    map: RwLock<HashMap<usize, u32>>,
}

impl PtrMap {
    fn new(name: &'static str) -> Self { Self { name, map: RwLock::new(HashMap::new()) } }

    fn get(&self, ptr: usize) -> Result<Option<u32>, u32> {
        match self.map.read() {
            Ok(map) => Ok(map.get(&ptr).copied()),
            Err(e) => {
                let name = self.name;
                log::error!("{name}: context map read lock poisoned: {e:?}");
                Err(CX_INTERNAL_ERROR)
            }
        }
    }

    fn insert(&self, ptr: usize, ctx_id: u32) -> Result<Option<u32>, u32> {
        match self.map.write() {
            Ok(mut map) => Ok(map.insert(ptr, ctx_id)),
            Err(e) => {
                let name = self.name;
                log::error!("{name}: context map write lock poisoned: {e:?}");
                Err(CX_INTERNAL_ERROR)
            }
        }
    }

    fn remove(&self, ptr: usize) -> Result<Option<u32>, u32> {
        match self.map.write() {
            Ok(mut map) => Ok(map.remove(&ptr)),
            Err(e) => {
                let name = self.name;
                log::error!("{name}: context map write lock poisoned: {e:?}");
                Err(CX_INTERNAL_ERROR)
            }
        }
    }
}

/// Mapping from C EC key pointer address to our internal context ID.
static KEY_PTR_MAP: LazyLock<PtrMap> = LazyLock::new(|| PtrMap::new("KEY_PTR_MAP"));

/// ABI-correct layout of `cx_ecfp_256_private_key_t` (C struct with `-fshort-enums`).
/// Uses `#[repr(C)]` so the compiler inserts correct padding for the target:
///   ARM (ILP32):   curve(1B) + pad(3B) + d_len(4B) + d[32] -> offset 8 for d
///   x86_64 (LP64): curve(1B) + pad(7B) + d_len(8B) + d[32] -> offset 16 for d
#[repr(C)]
pub struct CxEcfpPrivateKey {
    curve: u8,
    d_len: usize,
    d: [u8; 32],
}

/// ABI-correct layout of `cx_ecfp_256_public_key_t`.
#[repr(C)]
pub struct CxEcfpPublicKey {
    curve: u8,
    w_len: usize,
    w: [u8; 65],
}

// Shared Flux SDK hash/HMAC exports.

// --- SDK cx_md_t values (from lcx_hash.h) ---
const CX_NONE: u32 = 0;
const CX_SHA224: u32 = 2;
const CX_SHA256: u32 = 3;
const CX_SHA384: u32 = 4;
const CX_SHA512: u32 = 5;
const CX_KECCAK: u32 = 6;
const CX_SHA3: u32 = 7;

// --- SDK error codes (from cx_errors.h) ---
const CX_OK: u32 = 0x00000000;
const CX_INTERNAL_ERROR: u32 = 0xFFFFFF85;
const CX_INVALID_PARAMETER: u32 = 0xFFFFFF88;
const CX_INVALID_PARAMETER_SIZE: u32 = 0xFFFFFF89;

// --- SDK hash flags ---
const CX_FLAG_LAST: u32 = 0x0001;
const CX_FLAG_NO_REINIT: u32 = 0x0010;

/// Mapping from C hash context pointer address to our internal context ID.
/// The C code passes us a pointer to its own cx_hash_t struct; we use the
/// pointer value (as usize) as a key to look up our Rust hash context.
static HASH_PTR_MAP: LazyLock<PtrMap> = LazyLock::new(|| PtrMap::new("HASH_PTR_MAP"));

/// Mapping from C HMAC context pointer address to our internal context ID.
static HMAC_PTR_MAP: LazyLock<PtrMap> = LazyLock::new(|| PtrMap::new("HMAC_PTR_MAP"));

// ============================================================================
// Hash Functions
// ============================================================================

/// cx_keccak_init_no_throw(cx_sha3_t *hash, size_t size)
///
/// Initialize a Keccak hash context. `size` is the output size in bits (256).
#[no_mangle]
pub unsafe extern "C" fn cx_keccak_init_no_throw(hash: *mut u8, size: u32) -> u32 {
    if hash.is_null() || size != 256 {
        return CX_INVALID_PARAMETER;
    }

    let ctx_id = crate::hash::keccak_init();
    if let Err(err) = HASH_PTR_MAP.insert(hash as usize, ctx_id).map(|old_ctx| {
        if let Some(old_ctx) = old_ctx {
            let _ = crate::hash::hash_destroy(old_ctx);
        }
    }) {
        let _ = crate::hash::hash_destroy(ctx_id);
        return err;
    }

    log::debug!("cx_keccak_init_no_throw: hash={:p}, size={}, ctx_id={}", hash, size, ctx_id);
    CX_OK
}

/// cx_sha256_init_no_throw(cx_sha256_t *hash)
#[no_mangle]
pub unsafe extern "C" fn cx_sha256_init_no_throw(hash: *mut u8) -> u32 {
    if hash.is_null() {
        return CX_INVALID_PARAMETER;
    }

    let ctx_id = crate::hash::sha256_init();
    if let Err(err) = HASH_PTR_MAP.insert(hash as usize, ctx_id).map(|old_ctx| {
        if let Some(old_ctx) = old_ctx {
            let _ = crate::hash::hash_destroy(old_ctx);
        }
    }) {
        let _ = crate::hash::hash_destroy(ctx_id);
        return err;
    }

    log::debug!("cx_sha256_init_no_throw: hash={:p}, ctx_id={}", hash, ctx_id);
    CX_OK
}

/// cx_sha512_init_no_throw(cx_sha512_t *hash)
#[no_mangle]
pub unsafe extern "C" fn cx_sha512_init_no_throw(hash: *mut u8) -> u32 {
    if hash.is_null() {
        return CX_INVALID_PARAMETER;
    }

    let ctx_id = crate::hash::sha512_init();
    if let Err(err) = HASH_PTR_MAP.insert(hash as usize, ctx_id).map(|old_ctx| {
        if let Some(old_ctx) = old_ctx {
            let _ = crate::hash::hash_destroy(old_ctx);
        }
    }) {
        let _ = crate::hash::hash_destroy(ctx_id);
        return err;
    }

    log::debug!("cx_sha512_init_no_throw: hash={:p}, ctx_id={}", hash, ctx_id);
    CX_OK
}

/// cx_hash_no_throw(cx_hash_t *hash, uint32_t mode,
///     const uint8_t *in, size_t len, uint8_t *out, size_t out_len)
///
/// Unified hash function: update and/or finalize a hash context.
#[no_mangle]
pub unsafe extern "C" fn cx_hash_no_throw(
    hash_ctx: *mut u8,
    mode: u32,
    input: *const u8,
    len: u32,
    out: *mut u8,
    out_len: u32,
) -> u32 {
    if hash_ctx.is_null() {
        return CX_INVALID_PARAMETER;
    }

    let ctx_id = match HASH_PTR_MAP.get(hash_ctx as usize) {
        Ok(Some(id)) => id,
        Ok(None) => {
            log::warn!("cx_hash_no_throw: unknown hash context {:p}", hash_ctx);
            return CX_INVALID_PARAMETER;
        }
        Err(err) => return err,
    };

    // Update with input data if any
    if input.is_null() && len > 0 {
        return CX_INVALID_PARAMETER;
    }
    if len > 0 && !input.is_null() {
        let data = core::slice::from_raw_parts(input, len as usize);
        if let Err(e) = crate::hash::hash_update(ctx_id, data) {
            log::warn!("cx_hash_no_throw: update failed: {:?}", e);
            return CX_INTERNAL_ERROR;
        }
    }

    // Finalize if CX_LAST flag is set
    if mode & CX_FLAG_LAST != 0 {
        if out.is_null() {
            return CX_INVALID_PARAMETER;
        }

        let expected_len = match crate::hash::hash_info(ctx_id) {
            Ok((_, expected_len)) => expected_len,
            Err(e) => {
                log::warn!("cx_hash_no_throw: hash_info failed: {:?}", e);
                let _ = HASH_PTR_MAP.remove(hash_ctx as usize);
                return CX_INTERNAL_ERROR;
            }
        };
        if (out_len as usize) < expected_len {
            log::warn!("cx_hash_no_throw: output buffer too small: {} < {}", out_len, expected_len);
            return CX_INVALID_PARAMETER_SIZE;
        }

        match crate::hash::hash_final(ctx_id, mode) {
            Ok(digest) => {
                core::ptr::copy_nonoverlapping(digest.as_ptr(), out, digest.len());
                if mode & CX_FLAG_NO_REINIT != 0 {
                    let _ = HASH_PTR_MAP.remove(hash_ctx as usize);
                }
                log::debug!("cx_hash_no_throw: finalized ctx_id={}, digest_len={}", ctx_id, digest.len());
            }
            Err(e) => {
                log::warn!("cx_hash_no_throw: finalize failed: {:?}", e);
                let _ = HASH_PTR_MAP.remove(hash_ctx as usize);
                return CX_INTERNAL_ERROR;
            }
        }
    }

    CX_OK
}

/// cx_hash_update(cx_hash_t *ctx, const uint8_t *data, size_t len)
#[no_mangle]
pub unsafe extern "C" fn cx_hash_update(ctx: *mut u8, data: *const u8, len: u32) -> u32 {
    cx_hash_no_throw(ctx, 0, data, len, core::ptr::null_mut(), 0)
}

/// cx_hash_final(cx_hash_t *ctx, uint8_t *digest)
#[no_mangle]
pub unsafe extern "C" fn cx_hash_final(ctx: *mut u8, digest: *mut u8) -> u32 {
    // Use a large out_len since we don't know the algorithm; the implementation
    // will only write the correct number of bytes.
    cx_hash_no_throw(ctx, CX_FLAG_LAST, core::ptr::null(), 0, digest, 64)
}

/// cx_hash_init(cx_hash_t *ctx, cx_md_t md_type)
#[no_mangle]
pub unsafe extern "C" fn cx_hash_init(ctx: *mut u8, md_type: u32) -> u32 {
    match md_type {
        CX_SHA256 => cx_sha256_init_no_throw(ctx),
        CX_SHA512 => cx_sha512_init_no_throw(ctx),
        CX_KECCAK | CX_SHA3 => cx_keccak_init_no_throw(ctx, 256),
        _ => {
            log::warn!("cx_hash_init: unsupported md_type={}", md_type);
            CX_INVALID_PARAMETER
        }
    }
}

/// cx_hash_get_size(const cx_hash_t *ctx)
#[no_mangle]
pub unsafe extern "C" fn cx_hash_get_size(ctx: *const u8) -> u32 {
    if ctx.is_null() {
        return 0;
    }
    let ctx_id = match HASH_PTR_MAP.get(ctx as usize) {
        Ok(Some(id)) => id,
        Ok(None) | Err(_) => return 0,
    };
    match crate::hash::hash_info(ctx_id) {
        Ok((_, size)) => size as u32,
        Err(_) => 0,
    }
}

/// cx_hash_sha256(const uint8_t *in, size_t len, uint8_t *out, size_t out_len)
#[no_mangle]
pub unsafe extern "C" fn cx_hash_sha256(input: *const u8, len: u32, out: *mut u8, out_len: u32) -> u32 {
    if input.is_null() || out.is_null() {
        return CX_INVALID_PARAMETER;
    }
    if out_len < 32 {
        return CX_INVALID_PARAMETER_SIZE;
    }
    let data = core::slice::from_raw_parts(input, len as usize);
    let digest = crate::hash::sha256_oneshot(data);
    core::ptr::copy_nonoverlapping(digest.as_ptr(), out, 32);
    CX_OK
}

/// cx_hash_sha512(const uint8_t *in, size_t len, uint8_t *out, size_t out_len)
#[no_mangle]
pub unsafe extern "C" fn cx_hash_sha512(input: *const u8, len: u32, out: *mut u8, out_len: u32) -> u32 {
    if input.is_null() || out.is_null() {
        return CX_INVALID_PARAMETER;
    }
    if out_len < 64 {
        return CX_INVALID_PARAMETER_SIZE;
    }
    let data = core::slice::from_raw_parts(input, len as usize);
    let digest = crate::hash::sha512_oneshot(data);
    core::ptr::copy_nonoverlapping(digest.as_ptr(), out, 64);
    CX_OK
}

/// cx_sha256_update(cx_sha256_t *ctx, const uint8_t *data, size_t len)
#[no_mangle]
pub unsafe extern "C" fn cx_sha256_update(ctx: *mut u8, data: *const u8, len: u32) -> u32 {
    cx_hash_update(ctx, data, len)
}

/// cx_sha256_final(cx_sha256_t *ctx, uint8_t *digest)
#[no_mangle]
pub unsafe extern "C" fn cx_sha256_final(ctx: *mut u8, digest: *mut u8) -> u32 {
    cx_hash_no_throw(ctx, CX_FLAG_LAST, core::ptr::null(), 0, digest, 32)
}

/// cx_sha512_update(cx_sha512_t *ctx, const uint8_t *data, size_t len)
#[no_mangle]
pub unsafe extern "C" fn cx_sha512_update(ctx: *mut u8, data: *const u8, len: u32) -> u32 {
    cx_hash_update(ctx, data, len)
}

/// cx_sha512_final(cx_sha512_t *ctx, uint8_t *digest)
#[no_mangle]
pub unsafe extern "C" fn cx_sha512_final(ctx: *mut u8, digest: *mut u8) -> u32 {
    cx_hash_no_throw(ctx, CX_FLAG_LAST, core::ptr::null(), 0, digest, 64)
}

/// cx_sha3_update(cx_sha3_t *ctx, const uint8_t *data, size_t len)
#[no_mangle]
pub unsafe extern "C" fn cx_sha3_update(ctx: *mut u8, data: *const u8, len: u32) -> u32 {
    cx_hash_update(ctx, data, len)
}

/// cx_sha3_final(cx_sha3_t *ctx, uint8_t *digest)
#[no_mangle]
pub unsafe extern "C" fn cx_sha3_final(ctx: *mut u8, digest: *mut u8) -> u32 {
    cx_hash_no_throw(ctx, CX_FLAG_LAST, core::ptr::null(), 0, digest, 32)
}

/// cx_sha3_get_output_size(const cx_sha3_t *ctx)
#[no_mangle]
pub unsafe extern "C" fn cx_sha3_get_output_size(ctx: *const u8) -> u32 { cx_hash_get_size(ctx) }

/// cx_hash_get_info(cx_md_t md_type) -> const cx_hash_info_t*
/// Returns NULL since we don't use the info struct; lib_cxng code that
/// needs this won't be called since we override the high-level functions.
#[no_mangle]
pub extern "C" fn cx_hash_get_info(_md_type: u32) -> *const core::ffi::c_void { core::ptr::null() }

// ============================================================================
// HMAC Functions
// ============================================================================

/// cx_hmac_sha256_init_no_throw(cx_hmac_sha256_t *hmac, const uint8_t *key, size_t key_len)
#[no_mangle]
pub unsafe extern "C" fn cx_hmac_sha256_init_no_throw(
    hmac_ctx: *mut u8,
    key: *const u8,
    key_len: u32,
) -> u32 {
    if hmac_ctx.is_null() || (key.is_null() && key_len > 0) {
        return CX_INVALID_PARAMETER;
    }

    let key_data =
        if key_len > 0 && !key.is_null() { core::slice::from_raw_parts(key, key_len as usize) } else { &[] };

    let ctx_id = crate::hmac::hmac_sha256_init(key_data);
    if let Err(err) = HMAC_PTR_MAP.insert(hmac_ctx as usize, ctx_id).map(|old_ctx| {
        if let Some(old_ctx) = old_ctx {
            let _ = crate::hmac::hmac_destroy(old_ctx);
        }
    }) {
        let _ = crate::hmac::hmac_destroy(ctx_id);
        return err;
    }

    log::debug!("cx_hmac_sha256_init_no_throw: ctx={:p}, key_len={}, ctx_id={}", hmac_ctx, key_len, ctx_id);
    CX_OK
}

/// cx_hmac_sha512_init_no_throw(cx_hmac_sha512_t *hmac, const uint8_t *key, size_t key_len)
#[no_mangle]
pub unsafe extern "C" fn cx_hmac_sha512_init_no_throw(
    hmac_ctx: *mut u8,
    key: *const u8,
    key_len: u32,
) -> u32 {
    if hmac_ctx.is_null() || (key.is_null() && key_len > 0) {
        return CX_INVALID_PARAMETER;
    }

    let key_data =
        if key_len > 0 && !key.is_null() { core::slice::from_raw_parts(key, key_len as usize) } else { &[] };

    let ctx_id = crate::hmac::hmac_sha512_init(key_data);
    if let Err(err) = HMAC_PTR_MAP.insert(hmac_ctx as usize, ctx_id).map(|old_ctx| {
        if let Some(old_ctx) = old_ctx {
            let _ = crate::hmac::hmac_destroy(old_ctx);
        }
    }) {
        let _ = crate::hmac::hmac_destroy(ctx_id);
        return err;
    }

    log::debug!("cx_hmac_sha512_init_no_throw: ctx={:p}, key_len={}, ctx_id={}", hmac_ctx, key_len, ctx_id);
    CX_OK
}

/// cx_hmac_no_throw(cx_hmac_t *hmac, uint32_t mode,
///     const uint8_t *in, size_t len, uint8_t *mac, size_t mac_len)
#[no_mangle]
pub unsafe extern "C" fn cx_hmac_no_throw(
    hmac_ctx: *mut u8,
    mode: u32,
    input: *const u8,
    len: u32,
    mac: *mut u8,
    mac_len: u32,
) -> u32 {
    if hmac_ctx.is_null() {
        return CX_INVALID_PARAMETER;
    }

    let ctx_id = match HMAC_PTR_MAP.get(hmac_ctx as usize) {
        Ok(Some(id)) => id,
        Ok(None) => {
            log::warn!("cx_hmac_no_throw: unknown HMAC context {:p}", hmac_ctx);
            return CX_INVALID_PARAMETER;
        }
        Err(err) => return err,
    };

    // Update with input data if any
    if input.is_null() && len > 0 {
        return CX_INVALID_PARAMETER;
    }
    if len > 0 && !input.is_null() {
        let data = core::slice::from_raw_parts(input, len as usize);
        if let Err(e) = crate::hmac::hmac_update(ctx_id, data) {
            log::warn!("cx_hmac_no_throw: update failed: {:?}", e);
            return CX_INTERNAL_ERROR;
        }
    }

    // Finalize if CX_LAST flag is set
    if mode & CX_FLAG_LAST != 0 {
        if mac.is_null() {
            let _ = HMAC_PTR_MAP.remove(hmac_ctx as usize);
            let _ = crate::hmac::hmac_destroy(ctx_id);
            return CX_INVALID_PARAMETER;
        }

        match crate::hmac::hmac_final(ctx_id) {
            Ok(result) => {
                let _ = HMAC_PTR_MAP.remove(hmac_ctx as usize);
                if (mac_len as usize) < result.len() {
                    log::warn!("cx_hmac_no_throw: output buffer too small: {} < {}", mac_len, result.len());
                    return CX_INVALID_PARAMETER_SIZE;
                }
                core::ptr::copy_nonoverlapping(result.as_ptr(), mac, result.len());
            }
            Err(e) => {
                log::warn!("cx_hmac_no_throw: finalize failed: {:?}", e);
                let _ = HMAC_PTR_MAP.remove(hmac_ctx as usize);
                return CX_INTERNAL_ERROR;
            }
        }
    }

    CX_OK
}

/// cx_hmac_update(cx_hmac_t *ctx, const uint8_t *data, size_t len)
#[no_mangle]
pub unsafe extern "C" fn cx_hmac_update(ctx: *mut u8, data: *const u8, len: u32) -> u32 {
    cx_hmac_no_throw(ctx, 0, data, len, core::ptr::null_mut(), 0)
}

/// cx_hmac_final(cx_hmac_t *ctx, uint8_t *output)
#[no_mangle]
pub unsafe extern "C" fn cx_hmac_final(ctx: *mut u8, output: *mut u8) -> u32 {
    cx_hmac_no_throw(ctx, CX_FLAG_LAST, core::ptr::null(), 0, output, 64)
}

/// cx_hmac_init(cx_hmac_t *ctx, cx_md_t md_type, const uint8_t *key, size_t key_len)
#[no_mangle]
pub unsafe extern "C" fn cx_hmac_init(ctx: *mut u8, md_type: u32, key: *const u8, key_len: u32) -> u32 {
    match md_type {
        CX_SHA256 => cx_hmac_sha256_init_no_throw(ctx, key, key_len),
        CX_SHA512 => cx_hmac_sha512_init_no_throw(ctx, key, key_len),
        _ => {
            log::warn!("cx_hmac_init: unsupported md_type={}", md_type);
            CX_INVALID_PARAMETER
        }
    }
}

/// cx_hmac_sha256(const uint8_t *key, size_t key_len,
///     const uint8_t *in, size_t len, uint8_t *mac, size_t mac_len)
#[no_mangle]
pub unsafe extern "C" fn cx_hmac_sha256(
    key: *const u8,
    key_len: u32,
    input: *const u8,
    len: u32,
    mac: *mut u8,
    mac_len: u32,
) -> u32 {
    if (key.is_null() && key_len > 0) || (input.is_null() && len > 0) || mac.is_null() {
        return CX_INVALID_PARAMETER;
    }
    if mac_len < 32 {
        return CX_INVALID_PARAMETER_SIZE;
    }

    let key_data =
        if key_len > 0 && !key.is_null() { core::slice::from_raw_parts(key, key_len as usize) } else { &[] };
    let input_data =
        if len > 0 && !input.is_null() { core::slice::from_raw_parts(input, len as usize) } else { &[] };
    let ctx_id = crate::hmac::hmac_sha256_init(key_data);
    if let Err(e) = crate::hmac::hmac_update(ctx_id, input_data) {
        log::warn!("cx_hmac_sha256: update failed: {:?}", e);
        let _ = crate::hmac::hmac_destroy(ctx_id);
        return CX_INTERNAL_ERROR;
    }
    match crate::hmac::hmac_final(ctx_id) {
        Ok(result) => {
            core::ptr::copy_nonoverlapping(result.as_ptr(), mac, result.len());
            CX_OK
        }
        Err(e) => {
            log::warn!("cx_hmac_sha256: finalize failed: {:?}", e);
            CX_INTERNAL_ERROR
        }
    }
}

/// cx_hmac_sha512(const uint8_t *key, size_t key_len,
///     const uint8_t *in, size_t len, uint8_t *mac, size_t mac_len)
#[no_mangle]
pub unsafe extern "C" fn cx_hmac_sha512(
    key: *const u8,
    key_len: u32,
    input: *const u8,
    len: u32,
    mac: *mut u8,
    mac_len: u32,
) -> u32 {
    if (key.is_null() && key_len > 0) || (input.is_null() && len > 0) || mac.is_null() {
        return CX_INVALID_PARAMETER;
    }
    if mac_len < 64 {
        return CX_INVALID_PARAMETER_SIZE;
    }

    let key_data =
        if key_len > 0 && !key.is_null() { core::slice::from_raw_parts(key, key_len as usize) } else { &[] };
    let input_data =
        if len > 0 && !input.is_null() { core::slice::from_raw_parts(input, len as usize) } else { &[] };
    let ctx_id = crate::hmac::hmac_sha512_init(key_data);
    if let Err(e) = crate::hmac::hmac_update(ctx_id, input_data) {
        log::warn!("cx_hmac_sha512: update failed: {:?}", e);
        let _ = crate::hmac::hmac_destroy(ctx_id);
        return CX_INTERNAL_ERROR;
    }
    match crate::hmac::hmac_final(ctx_id) {
        Ok(result) => {
            core::ptr::copy_nonoverlapping(result.as_ptr(), mac, result.len());
            CX_OK
        }
        Err(e) => {
            log::warn!("cx_hmac_sha512: finalize failed: {:?}", e);
            CX_INTERNAL_ERROR
        }
    }
}

/// cx_hash_init_ex(cx_hash_t *ctx, cx_md_t md_type, size_t output_size)
///
/// The `_ex` variant takes an additional `output_size` parameter used by
/// variable-output hashes (SHA3/Keccak). Called by `hash_iovec_ex()` in
/// the Flux SDK's `cx_hash_iovec.c`.
#[no_mangle]
pub unsafe extern "C" fn cx_hash_init_ex(ctx: *mut u8, md_type: u32, output_size: u32) -> u32 {
    log::debug!("cx_hash_init_ex(ctx={:p}, md_type=0x{:02x}, output_size={})", ctx, md_type, output_size);
    let result = match md_type {
        CX_SHA256 => cx_sha256_init_no_throw(ctx),
        CX_SHA512 => cx_sha512_init_no_throw(ctx),
        CX_KECCAK | CX_SHA3 => cx_keccak_init_no_throw(ctx, output_size * 8),
        _ => {
            log::warn!("cx_hash_init_ex: unsupported md_type=0x{:02x}", md_type);
            CX_INVALID_PARAMETER
        }
    };
    log::debug!("cx_hash_init_ex -> result=0x{:x}", result);
    result
}

// ============================================================================
// EC Key Functions
// ============================================================================

/// cx_ecfp_init_private_key_no_throw(cx_curve_t curve,
///     const uint8_t *rawkey, size_t key_len, cx_ecfp_private_key_t *pvkey)
///
/// C struct cx_ecfp_256_private_key_s: { cx_curve_t curve; size_t d_len; uint8_t d[32]; }
#[no_mangle]
pub unsafe extern "C" fn cx_ecfp_init_private_key_no_throw(
    curve: u32,
    rawkey: *const u8,
    key_len: u32,
    pvkey: *mut CxEcfpPrivateKey,
) -> u32 {
    if pvkey.is_null() || (rawkey.is_null() && key_len > 0) {
        return CX_INVALID_PARAMETER;
    }

    let key_len = key_len as usize;
    if key_len > 32 {
        log::warn!("cx_ecfp_init_private_key_no_throw: invalid key_len={}", key_len);
        return CX_INVALID_PARAMETER_SIZE;
    }

    // Write curve, d_len, and d to the C struct via ABI-correct layout
    let key = &mut *pvkey;
    key.curve = curve as u8;
    key.d_len = key_len;

    if key_len > 0 && !rawkey.is_null() {
        core::ptr::copy_nonoverlapping(rawkey, key.d.as_mut_ptr(), key_len);
    }

    // Also register with our Rust EC context
    if key_len == 32 && !rawkey.is_null() {
        let raw = core::slice::from_raw_parts(rawkey, 32);
        let ctx_id = keys::init_private_key(curve as u8, raw);
        if ctx_id > 0 {
            match KEY_PTR_MAP.insert(pvkey as usize, ctx_id) {
                Ok(Some(old_ctx)) => {
                    let _ = keys::destroy_ec_context(old_ctx);
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = keys::destroy_ec_context(ctx_id);
                    return err;
                }
            }
        } else {
            let _ = KEY_PTR_MAP
                .remove(pvkey as usize)
                .map(|old_ctx| old_ctx.map(|old_ctx| keys::destroy_ec_context(old_ctx)));
            return CX_INVALID_PARAMETER;
        }
    } else if let Err(err) = KEY_PTR_MAP
        .remove(pvkey as usize)
        .map(|old_ctx| old_ctx.map(|old_ctx| keys::destroy_ec_context(old_ctx)))
    {
        return err;
    }

    log::debug!(
        "cx_ecfp_init_private_key_no_throw: curve=0x{:02x}, key_len={}, pvkey={:p}",
        curve,
        key_len,
        pvkey
    );
    CX_OK
}

/// cx_ecfp_init_public_key_no_throw(cx_curve_t curve,
///     const uint8_t *rawkey, size_t key_len, cx_ecfp_public_key_t *pukey)
///
/// C struct cx_ecfp_256_public_key_s: { cx_curve_t curve; size_t W_len; uint8_t W[65]; }
#[no_mangle]
pub unsafe extern "C" fn cx_ecfp_init_public_key_no_throw(
    curve: u32,
    rawkey: *const u8,
    key_len: u32,
    pukey: *mut CxEcfpPublicKey,
) -> u32 {
    if pukey.is_null() || (rawkey.is_null() && key_len > 0) {
        return CX_INVALID_PARAMETER;
    }

    let key_len = key_len as usize;
    if key_len > 65 {
        log::warn!("cx_ecfp_init_public_key_no_throw: invalid key_len={}", key_len);
        return CX_INVALID_PARAMETER_SIZE;
    }

    // Write curve, W_len, and W to the C struct via ABI-correct layout
    let key = &mut *pukey;
    key.curve = curve as u8;
    key.w_len = key_len;

    if key_len > 0 && !rawkey.is_null() {
        core::ptr::copy_nonoverlapping(rawkey, key.w.as_mut_ptr(), key_len);
    }

    log::debug!(
        "cx_ecfp_init_public_key_no_throw: curve=0x{:02x}, key_len={}, pukey={:p}",
        curve,
        key_len,
        pukey
    );
    CX_OK
}

/// cx_ecfp_generate_pair_no_throw(cx_curve_t curve,
///     cx_ecfp_public_key_t *pubkey, cx_ecfp_private_key_t *privkey, bool keepprivate)
#[no_mangle]
pub unsafe extern "C" fn cx_ecfp_generate_pair_no_throw(
    curve: u32,
    pubkey: *mut CxEcfpPublicKey,
    privkey: *const CxEcfpPrivateKey,
    _keepprivate: u32,
) -> u32 {
    if pubkey.is_null() || privkey.is_null() {
        return CX_INVALID_PARAMETER;
    }

    if curve != CX_CURVE_SECP256K1 && curve != CX_CURVE_ED25519 {
        log::warn!("cx_ecfp_generate_pair_no_throw: unsupported curve 0x{:02x}", curve);
        return CX_INVALID_PARAMETER;
    }

    // Read private key from C struct via ABI-correct layout
    let priv_key = &*privkey;
    let d_len = priv_key.d_len;
    if d_len != 32 {
        return CX_INVALID_PARAMETER_SIZE;
    }
    let d = &priv_key.d;

    // Create temporary context for key generation
    let ctx_id = keys::init_private_key(curve as u8, d);
    if ctx_id == 0 {
        return CX_INVALID_PARAMETER;
    }

    if let Err(e) = keys::generate_pair(ctx_id, false) {
        log::warn!("cx_ecfp_generate_pair_no_throw: generate_pair failed: {:?}", e);
        let _ = keys::destroy_ec_context(ctx_id);
        return CX_INTERNAL_ERROR;
    }

    match keys::get_public_key(ctx_id) {
        Ok(pk) => {
            // Write public key to C struct via ABI-correct layout
            let pub_key = &mut *pubkey;
            if pk.len() > pub_key.w.len() {
                log::warn!("cx_ecfp_generate_pair_no_throw: public key too large: {}", pk.len());
                let _ = keys::destroy_ec_context(ctx_id);
                return CX_INTERNAL_ERROR;
            }
            pub_key.curve = curve as u8;
            pub_key.w_len = pk.len();
            core::ptr::copy_nonoverlapping(pk.as_ptr(), pub_key.w.as_mut_ptr(), pk.len());

            // Store mapping for later use in sign/verify
            match KEY_PTR_MAP.insert(privkey as usize, ctx_id) {
                Ok(Some(old_ctx)) => {
                    let _ = keys::destroy_ec_context(old_ctx);
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = keys::destroy_ec_context(ctx_id);
                    return err;
                }
            }

            log::debug!("cx_ecfp_generate_pair_no_throw: pubkey_len={}, privkey={:p}", pk.len(), privkey);
            CX_OK
        }
        Err(e) => {
            log::warn!("cx_ecfp_generate_pair_no_throw: get_public_key failed: {:?}", e);
            let _ = keys::destroy_ec_context(ctx_id);
            CX_INTERNAL_ERROR
        }
    }
}

/// cx_ecfp_generate_pair2_no_throw(cx_curve_t curve,
///     cx_ecfp_public_key_t *pubkey, cx_ecfp_private_key_t *privkey,
///     bool keepprivate, cx_md_t hashID)
///
/// The "2" variant adds a hashID parameter (used for Ed25519 with CX_SHA512).
/// For both secp256k1 and Ed25519, delegate to the base version.
#[no_mangle]
pub unsafe extern "C" fn cx_ecfp_generate_pair2_no_throw(
    curve: u32,
    pubkey: *mut CxEcfpPublicKey,
    privkey: *const CxEcfpPrivateKey,
    keepprivate: u32,
    _hash_id: u32,
) -> u32 {
    log::debug!("cx_ecfp_generate_pair2_no_throw: curve=0x{:02x}, hash_id=0x{:02x}", curve, _hash_id);
    cx_ecfp_generate_pair_no_throw(curve, pubkey, privkey, keepprivate)
}

// ============================================================================
// ECDSA Functions
// ============================================================================

/// cx_ecdsa_sign_no_throw(const cx_ecfp_private_key_t *pvkey,
///     uint32_t mode, cx_md_t hashID,
///     const uint8_t *hash, size_t hash_len,
///     uint8_t *sig, size_t *sig_len, uint32_t *info)
#[no_mangle]
pub unsafe extern "C" fn cx_ecdsa_sign_no_throw(
    pvkey: *const CxEcfpPrivateKey,
    _mode: u32,
    _hash_id: u32,
    hash: *const u8,
    hash_len: u32,
    sig: *mut u8,
    sig_len: *mut u32,
    info: *mut u32,
) -> u32 {
    if pvkey.is_null() || hash.is_null() || sig.is_null() || sig_len.is_null() {
        return CX_INVALID_PARAMETER;
    }

    // Read private key from C struct
    let priv_key = &*pvkey;
    let curve = priv_key.curve as u32;
    let d_len = priv_key.d_len;
    if curve != CX_CURVE_SECP256K1 || d_len != 32 {
        return CX_INVALID_PARAMETER;
    }
    let d = &priv_key.d;

    // Get or create EC context
    let ctx_id = match KEY_PTR_MAP.get(pvkey as usize) {
        Ok(ctx_id) => ctx_id,
        Err(err) => return err,
    };

    let ctx_id = match ctx_id {
        Some(id) => id,
        None => {
            let id = keys::init_private_key(curve as u8, d);
            if id == 0 {
                return CX_INTERNAL_ERROR;
            }
            match KEY_PTR_MAP.insert(pvkey as usize, id) {
                Ok(Some(old_ctx)) => {
                    let _ = keys::destroy_ec_context(old_ctx);
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = keys::destroy_ec_context(id);
                    return err;
                }
            }
            id
        }
    };

    // Sign
    let hash_data = core::slice::from_raw_parts(hash, hash_len as usize);
    // Pad or truncate hash to 32 bytes for secp256k1
    let mut hash32 = [0u8; 32];
    let copy_len = hash_data.len().min(32);
    hash32[32 - copy_len..].copy_from_slice(&hash_data[..copy_len]);

    match keys::ecdsa_sign_recoverable(ctx_id, &hash32) {
        Ok((der_sig, sig_info)) => {
            let out_len = der_sig.len().min(*sig_len as usize);
            core::ptr::copy_nonoverlapping(der_sig.as_ptr(), sig, out_len);
            *sig_len = out_len as u32;
            if !info.is_null() {
                *info = sig_info;
            }
            log::debug!("cx_ecdsa_sign_no_throw: sig_len={}, info=0x{:02x}", out_len, sig_info);
            CX_OK
        }
        Err(e) => {
            log::warn!("cx_ecdsa_sign_no_throw: sign failed: {:?}", e);
            CX_INTERNAL_ERROR
        }
    }
}

/// cx_ecdsa_sign_rs_no_throw — sign and return raw (r, s) instead of DER
///
/// Same as cx_ecdsa_sign_no_throw but writes r and s as fixed-width big-endian
/// byte arrays of `rs_len` bytes each (typically 32 for secp256k1).
#[no_mangle]
pub unsafe extern "C" fn cx_ecdsa_sign_rs_no_throw(
    pvkey: *const CxEcfpPrivateKey,
    _mode: u32,
    _hash_id: u32,
    hash: *const u8,
    hash_len: u32,
    rs_len: u32,
    sig_r: *mut u8,
    sig_s: *mut u8,
    info: *mut u32,
) -> u32 {
    if pvkey.is_null() || hash.is_null() || sig_r.is_null() || sig_s.is_null() {
        return CX_INVALID_PARAMETER;
    }
    let rs_len = rs_len as usize;

    let priv_key = &*pvkey;
    let curve = priv_key.curve as u32;
    let d_len = priv_key.d_len;
    if curve != CX_CURVE_SECP256K1 || d_len != 32 {
        return CX_INVALID_PARAMETER;
    }
    let d = &priv_key.d;

    let ctx_id = match KEY_PTR_MAP.get(pvkey as usize) {
        Ok(ctx_id) => ctx_id,
        Err(err) => return err,
    };
    let ctx_id = match ctx_id {
        Some(id) => id,
        None => {
            let id = keys::init_private_key(curve as u8, d);
            if id == 0 {
                return CX_INTERNAL_ERROR;
            }
            match KEY_PTR_MAP.insert(pvkey as usize, id) {
                Ok(Some(old_ctx)) => {
                    let _ = keys::destroy_ec_context(old_ctx);
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = keys::destroy_ec_context(id);
                    return err;
                }
            }
            id
        }
    };

    let hash_data = core::slice::from_raw_parts(hash, hash_len as usize);
    let mut hash32 = [0u8; 32];
    let copy_len = hash_data.len().min(32);
    hash32[32 - copy_len..].copy_from_slice(&hash_data[..copy_len]);

    match keys::ecdsa_sign_recoverable(ctx_id, &hash32) {
        Ok((der_sig, sig_info)) => {
            // Parse DER: 30 <total_len> 02 <r_len> <r_bytes...> 02 <s_len> <s_bytes...>
            if der_sig.len() < 6 || der_sig[0] != 0x30 || der_sig[2] != 0x02 {
                log::error!("cx_ecdsa_sign_rs_no_throw: invalid DER signature");
                return CX_INTERNAL_ERROR;
            }
            let r_len = der_sig[3] as usize;
            let r_start = 4;
            if r_start + r_len + 2 > der_sig.len() {
                return CX_INTERNAL_ERROR;
            }
            let s_len = der_sig[r_start + r_len + 1] as usize;
            let s_start = r_start + r_len + 2;
            if s_start + s_len > der_sig.len() {
                return CX_INTERNAL_ERROR;
            }

            let out_r = core::slice::from_raw_parts_mut(sig_r, rs_len);
            let out_s = core::slice::from_raw_parts_mut(sig_s, rs_len);
            out_r.fill(0);
            out_s.fill(0);

            let r_bytes = &der_sig[r_start..r_start + r_len];
            let r_trim = if r_len > 0 && r_bytes[0] == 0 { &r_bytes[1..] } else { r_bytes };
            let copy = r_trim.len().min(rs_len);
            out_r[rs_len - copy..].copy_from_slice(&r_trim[r_trim.len() - copy..]);

            let s_bytes = &der_sig[s_start..s_start + s_len];
            let s_trim = if s_len > 0 && s_bytes[0] == 0 { &s_bytes[1..] } else { s_bytes };
            let copy = s_trim.len().min(rs_len);
            out_s[rs_len - copy..].copy_from_slice(&s_trim[s_trim.len() - copy..]);

            if !info.is_null() {
                *info = sig_info;
            }
            log::debug!("cx_ecdsa_sign_rs_no_throw: rs_len={}, info=0x{:02x}", rs_len, sig_info);
            CX_OK
        }
        Err(e) => {
            log::warn!("cx_ecdsa_sign_rs_no_throw: sign failed: {:?}", e);
            CX_INTERNAL_ERROR
        }
    }
}

/// cx_ecdsa_verify_no_throw(const cx_ecfp_public_key_t *pukey,
///     const uint8_t *hash, size_t hash_len,
///     const uint8_t *sig, size_t sig_len)
#[no_mangle]
pub unsafe extern "C" fn cx_ecdsa_verify_no_throw(
    pukey: *const CxEcfpPublicKey,
    hash: *const u8,
    hash_len: u32,
    sig: *const u8,
    sig_len: u32,
) -> u32 {
    if pukey.is_null() || hash.is_null() || sig.is_null() {
        return 0; // false
    }

    // Read public key from C struct via ABI-correct layout
    let pub_key = &*pukey;
    let curve = pub_key.curve as u32;
    let w_len = pub_key.w_len;
    if curve != CX_CURVE_SECP256K1 {
        return 0;
    }
    if w_len > pub_key.w.len() {
        log::warn!("cx_ecdsa_verify_no_throw: invalid w_len={}", w_len);
        return 0;
    }
    let w = &pub_key.w[..w_len];

    let ctx_id = keys::init_public_key(curve as u8, w);
    if ctx_id == 0 {
        return 0;
    }

    let hash_data = core::slice::from_raw_parts(hash, hash_len as usize);
    let mut hash32 = [0u8; 32];
    let copy_len = hash_data.len().min(32);
    hash32[32 - copy_len..].copy_from_slice(&hash_data[..copy_len]);

    let sig_data = core::slice::from_raw_parts(sig, sig_len as usize);
    let result = keys::ecdsa_verify(ctx_id, &hash32, sig_data);
    let _ = keys::destroy_ec_context(ctx_id);

    log::debug!("cx_ecdsa_verify_no_throw: result={}", result);
    if result {
        1
    } else {
        0
    }
}

// ============================================================================
// CRC32 Functions
// ============================================================================

/// cx_crc32(const uint8_t *buf, size_t len) -> uint32_t
#[no_mangle]
pub unsafe extern "C" fn cx_crc32(buf: *const u8, len: u32) -> u32 {
    if buf.is_null() {
        return 0;
    }
    let data = core::slice::from_raw_parts(buf, len as usize);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

/// cx_crc32_update(uint32_t crc, const uint8_t *buf, size_t len) -> uint32_t
#[no_mangle]
pub unsafe extern "C" fn cx_crc32_update(crc: u32, buf: *const u8, len: u32) -> u32 {
    if buf.is_null() {
        return crc;
    }
    let data = core::slice::from_raw_parts(buf, len as usize);
    let mut hasher = crc32fast::Hasher::new_with_initial(crc);
    hasher.update(data);
    hasher.finalize()
}

/// cx_crc_hw(uint32_t crc_type, uint32_t crc_state, const uint8_t *buf, size_t len) -> uint32_t
#[no_mangle]
pub unsafe extern "C" fn cx_crc_hw(_crc_type: u32, crc_state: u32, buf: *const u8, len: u32) -> u32 {
    cx_crc32_update(crc_state, buf, len)
}

// ============================================================================
// Misc Crypto Utility Functions
// ============================================================================

/// cx_memxor(void *buf1, const void *buf2, size_t len) -> void*
#[no_mangle]
pub unsafe extern "C" fn cx_memxor(buf1: *mut u8, buf2: *const u8, len: u32) -> *mut u8 {
    if buf1.is_null() || buf2.is_null() {
        return buf1;
    }
    let a = core::slice::from_raw_parts_mut(buf1, len as usize);
    let b = core::slice::from_raw_parts(buf2, len as usize);
    for (x, y) in a.iter_mut().zip(b.iter()) {
        *x ^= y;
    }
    buf1
}

/// cx_constant_time_eq(const void *buf1, const void *buf2, size_t len) -> bool
#[no_mangle]
pub unsafe extern "C" fn cx_constant_time_eq(buf1: *const u8, buf2: *const u8, len: u32) -> u32 {
    if buf1.is_null() || buf2.is_null() {
        return 0;
    }
    let a = core::slice::from_raw_parts(buf1, len as usize);
    let b = core::slice::from_raw_parts(buf2, len as usize);
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    if diff == 0 {
        1
    } else {
        0
    }
}

/// cx_swap_uint32(uint32_t v) -> uint32_t
#[no_mangle]
pub extern "C" fn cx_swap_uint32(v: u32) -> u32 { v.swap_bytes() }

/// cx_swap_uint64(uint64_t v) -> uint64_t
#[no_mangle]
pub extern "C" fn cx_swap_uint64(v: u64) -> u64 { v.swap_bytes() }

/// cx_swap_buffer32(uint32_t *buf, size_t len)
#[no_mangle]
pub unsafe extern "C" fn cx_swap_buffer32(buf: *mut u32, len: u32) {
    if buf.is_null() {
        return;
    }
    for i in 0..len as usize {
        let p = buf.add(i);
        *p = (*p).swap_bytes();
    }
}

/// cx_swap_buffer64(uint64_t *buf, size_t len)
#[no_mangle]
pub unsafe extern "C" fn cx_swap_buffer64(buf: *mut u64, len: u32) {
    if buf.is_null() {
        return;
    }
    for i in 0..len as usize {
        let p = buf.add(i);
        *p = (*p).swap_bytes();
    }
}

// ============================================================================
// BIP32 Key Derivation
// ============================================================================

/// os_perso_derive_node_bip32(cx_curve_t curve, const uint32_t *path,
///     size_t path_len, uint8_t *private_key, uint8_t *chain)
///
/// Derives a BIP32 key using the app seed stored on the FluxServer side.
/// This goes through IPC since the seed is not available in the app process.
#[no_mangle]
pub unsafe extern "C" fn os_perso_derive_node_bip32(
    _curve: u32,
    path: *const u32,
    path_len: u32,
    private_key: *mut u8,
    chain: *mut u8,
) -> u32 {
    if path.is_null() || private_key.is_null() {
        return CX_INVALID_PARAMETER;
    }

    let path_len = path_len as usize;
    if path_len == 0 || path_len > 10 {
        log::warn!("os_perso_derive_node_bip32: invalid path_len={}", path_len);
        return CX_INVALID_PARAMETER;
    }

    // Build a buffer containing the path (big-endian u32 elements)
    // Buffer layout: [path_element_0 (4 bytes BE), path_element_1 (4 bytes BE), ...]
    // Output will be: [private_key (32 bytes), chain_code (32 bytes)]
    let mut buf = vec![0u8; 64.max(path_len * 4)];
    for i in 0..path_len {
        let element = *path.add(i);
        buf[i * 4..i * 4 + 4].copy_from_slice(&element.to_be_bytes());
    }

    // Send via IPC to FluxServer which has the app seed
    let result = syscall_buffer(syscall_id::SYSCALL_OS_PERSO_DERIVE_NODE_BIP32_ID, path_len as u32, &mut buf);

    if result != 0 {
        log::warn!("os_perso_derive_node_bip32: IPC failed (result={})", result);
        return CX_INTERNAL_ERROR;
    }

    // Read output: private_key (32 bytes) + chain_code (32 bytes)
    core::ptr::copy_nonoverlapping(buf.as_ptr(), private_key, 32);
    if !chain.is_null() {
        core::ptr::copy_nonoverlapping(buf[32..].as_ptr(), chain, 32);
    }

    log::debug!("os_perso_derive_node_bip32: derived key for path_len={}", path_len);
    CX_OK
}

/// os_perso_derive_node_with_seed_key(unsigned int mode, cx_curve_t curve,
///     const uint32_t *path, size_t path_len,
///     uint8_t *private_key, uint8_t *chain,
///     const uint8_t *seed_key, size_t seed_key_len)
///
/// Derives a key with mode-dependent algorithm:
/// - mode 0 (HDW_NORMAL): BIP32 derivation (secp256k1)
/// - mode 1 (HDW_ED25519_SLIP10): SLIP-10 derivation (Ed25519)
#[no_mangle]
pub unsafe extern "C" fn os_perso_derive_node_with_seed_key(
    mode: u32,
    curve: u32,
    path: *const u32,
    path_len: u32,
    private_key: *mut u8,
    chain: *mut u8,
    _seed_key: *const u8,
    _seed_key_len: u32,
) -> u32 {
    if curve == CX_CURVE_SECP256K1 {
        return os_perso_derive_node_bip32(curve, path, path_len, private_key, chain);
    }

    if curve != CX_CURVE_ED25519 {
        log::warn!("os_perso_derive_node_with_seed_key: unsupported curve=0x{:02x}", curve);
        return CX_INVALID_PARAMETER;
    }

    if path.is_null() || private_key.is_null() {
        return CX_INVALID_PARAMETER;
    }

    let path_len_usize = path_len as usize;
    if path_len_usize == 0 || path_len_usize > 10 {
        log::warn!("os_perso_derive_node_with_seed_key: invalid path_len={}", path_len);
        return CX_INVALID_PARAMETER;
    }

    let mut buf = vec![0u8; 64.max(path_len_usize * 4)];
    for i in 0..path_len_usize {
        let element = *path.add(i);
        buf[i * 4..i * 4 + 4].copy_from_slice(&element.to_be_bytes());
    }

    // Encode mode + curve + path_len into the arg parameter:
    //   bits [31:24] = mode, bits [23:16] = curve, bits [15:0] = path_len
    let arg = ((mode & 0xFF) << 24) | ((curve & 0xFF) << 16) | (path_len_usize as u32 & 0xFFFF);

    log::debug!(
        "os_perso_derive_node_with_seed_key: mode={}, curve=0x{:02x}, path_len={}",
        mode,
        curve,
        path_len_usize
    );

    let result = syscall_buffer(syscall_id::SYSCALL_OS_PERSO_DERIVE_NODE_WITH_SEED_KEY_ID, arg, &mut buf);

    if result != 0 {
        log::warn!("os_perso_derive_node_with_seed_key: IPC failed (result={})", result);
        return CX_INTERNAL_ERROR;
    }

    core::ptr::copy_nonoverlapping(buf.as_ptr(), private_key, 32);
    if !chain.is_null() {
        core::ptr::copy_nonoverlapping(buf[32..].as_ptr(), chain, 32);
    }

    log::debug!("os_perso_derive_node_with_seed_key: derived key successfully");
    CX_OK
}

// ============================================================================
// EC Domain Information
// ============================================================================

/// cx_ecdomain_size(cx_curve_t curve, size_t *length)
///
/// Returns the byte-length of the base field of the curve.
#[no_mangle]
pub unsafe extern "C" fn cx_ecdomain_size(curve: u32, length: *mut u32) -> u32 {
    if length.is_null() {
        return CX_INVALID_PARAMETER;
    }
    match curve {
        CX_CURVE_SECP256K1 | CX_CURVE_ED25519 | CX_CURVE_Curve25519 => {
            *length = 32;
            CX_OK
        }
        _ => {
            log::warn!("cx_ecdomain_size: unsupported curve 0x{:02x}", curve);
            CX_INVALID_PARAMETER
        }
    }
}

/// cx_ecdomain_parameters_length(cx_curve_t curve, size_t *length)
///
/// Returns the byte-length of curve parameters.
#[no_mangle]
pub unsafe extern "C" fn cx_ecdomain_parameters_length(curve: u32, length: *mut u32) -> u32 {
    cx_ecdomain_size(curve, length)
}

/// cx_ecdomain_generator(cx_curve_t curve, uint8_t *Gx, uint8_t *Gy, size_t len)
///
/// Returns the generator point (uncompressed) for the curve.
#[no_mangle]
pub unsafe extern "C" fn cx_ecdomain_generator(curve: u32, gx: *mut u8, gy: *mut u8, len: u32) -> u32 {
    if curve != CX_CURVE_SECP256K1 {
        return CX_INVALID_PARAMETER;
    }
    if len < 32 {
        return CX_INVALID_PARAMETER_SIZE;
    }

    // secp256k1 generator point G coordinates
    static GX: [u8; 32] = [
        0x79, 0xBE, 0x66, 0x7E, 0xF9, 0xDC, 0xBB, 0xAC, 0x55, 0xA0, 0x62, 0x95, 0xCE, 0x87, 0x0B, 0x07, 0x02,
        0x9B, 0xFC, 0xDB, 0x2D, 0xCE, 0x28, 0xD9, 0x59, 0xF2, 0x81, 0x5B, 0x16, 0xF8, 0x17, 0x98,
    ];
    static GY: [u8; 32] = [
        0x48, 0x3A, 0xDA, 0x77, 0x26, 0xA3, 0xC4, 0x65, 0x5D, 0xA4, 0xFB, 0xFC, 0x0E, 0x11, 0x08, 0xA8, 0xFD,
        0x17, 0xB4, 0x48, 0xA6, 0x85, 0x54, 0x19, 0x9C, 0x47, 0xD0, 0x8F, 0xFB, 0x10, 0xD4, 0xB8,
    ];

    if !gx.is_null() {
        core::ptr::copy_nonoverlapping(GX.as_ptr(), gx, 32);
    }
    if !gy.is_null() {
        core::ptr::copy_nonoverlapping(GY.as_ptr(), gy, 32);
    }
    CX_OK
}

/// cx_ecdomain_parameter(cx_curve_t curve, cx_curve_dom_param_t id,
///     uint8_t *param, uint32_t param_len)
///
/// Returns a specific domain parameter for the curve.
#[no_mangle]
pub unsafe extern "C" fn cx_ecdomain_parameter(curve: u32, id: u32, param: *mut u8, param_len: u32) -> u32 {
    if curve != CX_CURVE_SECP256K1 || param.is_null() || param_len < 32 {
        return CX_INVALID_PARAMETER;
    }

    // cx_curve_dom_param_t values:
    // CX_CURVE_PARAM_A = 1, CX_CURVE_PARAM_B = 2, CX_CURVE_PARAM_Field = 3,
    // CX_CURVE_PARAM_Gx = 4, CX_CURVE_PARAM_Gy = 5, CX_CURVE_PARAM_Order = 6,
    // CX_CURVE_PARAM_Cofactor = 8
    match id {
        3 => {
            // Field prime p for secp256k1: FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
            let p: [u8; 32] = [
                0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0xFF, 0xFF,
                0xFC, 0x2F,
            ];
            core::ptr::copy_nonoverlapping(p.as_ptr(), param, 32);
            CX_OK
        }
        6 => {
            // Order n for secp256k1: FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
            let n: [u8; 32] = [
                0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                0xFE, 0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36,
                0x41, 0x41,
            ];
            core::ptr::copy_nonoverlapping(n.as_ptr(), param, 32);
            CX_OK
        }
        1 => {
            // a = 0 for secp256k1
            core::ptr::write_bytes(param, 0, 32);
            CX_OK
        }
        2 => {
            // b = 7 for secp256k1
            core::ptr::write_bytes(param, 0, 32);
            *param.add(31) = 7;
            CX_OK
        }
        _ => {
            log::warn!("cx_ecdomain_parameter: unsupported param id={}", id);
            CX_INVALID_PARAMETER
        }
    }
}

// ============================================================================
// EdDSA (Ed25519) Functions
// ============================================================================

/// cx_eddsa_sign_no_throw(const cx_ecfp_private_key_t *pvkey,
///     cx_md_t hashID,
///     const uint8_t *hash, size_t hash_len,
///     uint8_t *sig, size_t sig_len)
///
/// Signs a message using Ed25519. The `hash` parameter is the raw message
/// (Ed25519 hashes internally with SHA-512).
#[no_mangle]
pub unsafe extern "C" fn cx_eddsa_sign_no_throw(
    pvkey: *const CxEcfpPrivateKey,
    _hash_id: u32,
    hash: *const u8,
    hash_len: u32,
    sig: *mut u8,
    sig_len: u32,
) -> u32 {
    if pvkey.is_null() || hash.is_null() || sig.is_null() {
        return CX_INVALID_PARAMETER;
    }

    if sig_len < 64 {
        return CX_INVALID_PARAMETER_SIZE;
    }

    let priv_key = &*pvkey;
    let curve = priv_key.curve as u32;
    let d_len = priv_key.d_len;
    if curve != CX_CURVE_ED25519 || d_len != 32 {
        log::warn!("cx_eddsa_sign_no_throw: expected Ed25519, got curve=0x{:02x} d_len={}", curve, d_len);
        return CX_INVALID_PARAMETER;
    }
    let d = priv_key.d;

    let message = core::slice::from_raw_parts(hash, hash_len as usize);

    match keys::eddsa_sign(&d, message) {
        Ok(signature) => {
            core::ptr::copy_nonoverlapping(signature.as_ptr(), sig, 64);
            log::debug!("cx_eddsa_sign_no_throw: signed {} bytes, sig={:02x?}", hash_len, &signature[..4]);
            CX_OK
        }
        Err(e) => {
            log::warn!("cx_eddsa_sign_no_throw: sign failed: {:?}", e);
            CX_INTERNAL_ERROR
        }
    }
}

// ============================================================================
// Modular Arithmetic (cx_math_*)
// ============================================================================

/// Ed25519 field prime p = 2^255 - 19.
static ED25519_P: LazyLock<BigUint> = LazyLock::new(|| (BigUint::one() << 255) - BigUint::from(19u32));

/// Ed25519 curve parameter d = -121665/121666 mod p.
static ED25519_D: LazyLock<BigUint> = LazyLock::new(|| {
    let p = &*ED25519_P;
    let d_num = p - BigUint::from(121665u32);
    let d_denom_inv = BigUint::from(121666u32).modpow(&(p - BigUint::from(2u32)), p);
    (&d_num * &d_denom_inv) % p
});

/// sqrt(-1) mod p = 2^((p-1)/4) mod p.
static ED25519_SQRT_M1: LazyLock<BigUint> = LazyLock::new(|| {
    let p = &*ED25519_P;
    BigUint::from(2u32).modpow(&((p - BigUint::one()) / BigUint::from(4u32)), p)
});

/// Convert big-endian byte slice to BigUint.
fn be_to_biguint(data: &[u8]) -> BigUint { BigUint::from_bytes_be(data) }

/// Convert BigUint to big-endian bytes, zero-padded to `len`.
fn biguint_to_be_padded(val: &BigUint, len: usize) -> Vec<u8> {
    let bytes = val.to_bytes_be();
    if bytes.len() >= len {
        bytes[bytes.len() - len..].to_vec()
    } else {
        let mut result = vec![0u8; len];
        result[len - bytes.len()..].copy_from_slice(&bytes);
        result
    }
}

/// cx_math_modm_no_throw(uint8_t *v, size_t len_v, const uint8_t *m, size_t len_m)
///
/// Computes v = v mod m (in-place). Big-endian byte arrays.
#[no_mangle]
pub unsafe extern "C" fn cx_math_modm_no_throw(v: *mut u8, len_v: u32, m: *const u8, len_m: u32) -> u32 {
    if v.is_null() || m.is_null() || len_m == 0 {
        return CX_INVALID_PARAMETER;
    }
    let v_big = be_to_biguint(core::slice::from_raw_parts(v, len_v as usize));
    let m_big = be_to_biguint(core::slice::from_raw_parts(m, len_m as usize));
    if m_big.is_zero() {
        return CX_INVALID_PARAMETER;
    }
    let result = v_big % &m_big;
    let result_bytes = biguint_to_be_padded(&result, len_v as usize);
    core::ptr::copy_nonoverlapping(result_bytes.as_ptr(), v, len_v as usize);
    CX_OK
}

/// cx_math_addm_no_throw — r = (a + b) mod m. Big-endian, all `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn cx_math_addm_no_throw(
    r: *mut u8,
    a: *const u8,
    b: *const u8,
    m: *const u8,
    len: u32,
) -> u32 {
    if r.is_null() || a.is_null() || b.is_null() || m.is_null() || len == 0 {
        return CX_INVALID_PARAMETER;
    }
    let len = len as usize;
    let a_big = be_to_biguint(core::slice::from_raw_parts(a, len));
    let b_big = be_to_biguint(core::slice::from_raw_parts(b, len));
    let m_big = be_to_biguint(core::slice::from_raw_parts(m, len));
    if m_big.is_zero() {
        return CX_INVALID_PARAMETER;
    }
    let result = (&a_big + &b_big) % &m_big;
    let result_bytes = biguint_to_be_padded(&result, len);
    core::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, len);
    CX_OK
}

/// cx_math_subm_no_throw — r = (a - b) mod m. Big-endian, all `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn cx_math_subm_no_throw(
    r: *mut u8,
    a: *const u8,
    b: *const u8,
    m: *const u8,
    len: u32,
) -> u32 {
    if r.is_null() || a.is_null() || b.is_null() || m.is_null() || len == 0 {
        return CX_INVALID_PARAMETER;
    }
    let len = len as usize;
    let a_big = be_to_biguint(core::slice::from_raw_parts(a, len));
    let b_big = be_to_biguint(core::slice::from_raw_parts(b, len));
    let m_big = be_to_biguint(core::slice::from_raw_parts(m, len));
    if m_big.is_zero() {
        return CX_INVALID_PARAMETER;
    }
    // Add m to avoid BigUint underflow: (a + m - b) mod m = (a - b) mod m
    let result = (&a_big + &m_big - &b_big) % &m_big;
    let result_bytes = biguint_to_be_padded(&result, len);
    core::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, len);
    CX_OK
}

/// cx_math_multm_no_throw — r = (a * b) mod m. Big-endian, all `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn cx_math_multm_no_throw(
    r: *mut u8,
    a: *const u8,
    b: *const u8,
    m: *const u8,
    len: u32,
) -> u32 {
    if r.is_null() || a.is_null() || b.is_null() || m.is_null() || len == 0 {
        return CX_INVALID_PARAMETER;
    }
    let len = len as usize;
    let a_big = be_to_biguint(core::slice::from_raw_parts(a, len));
    let b_big = be_to_biguint(core::slice::from_raw_parts(b, len));
    let m_big = be_to_biguint(core::slice::from_raw_parts(m, len));
    if m_big.is_zero() {
        return CX_INVALID_PARAMETER;
    }
    let result = (&a_big * &b_big) % &m_big;
    let result_bytes = biguint_to_be_padded(&result, len);
    core::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, len);
    CX_OK
}

/// cx_math_powm_no_throw — r = a^e mod m. Big-endian.
/// a, r, m are `len` bytes; e is `len_e` bytes.
#[no_mangle]
pub unsafe extern "C" fn cx_math_powm_no_throw(
    r: *mut u8,
    a: *const u8,
    e: *const u8,
    len_e: u32,
    m: *const u8,
    len: u32,
) -> u32 {
    if r.is_null() || a.is_null() || e.is_null() || m.is_null() || len == 0 || len_e == 0 {
        return CX_INVALID_PARAMETER;
    }
    let len = len as usize;
    let a_big = be_to_biguint(core::slice::from_raw_parts(a, len));
    let e_big = be_to_biguint(core::slice::from_raw_parts(e, len_e as usize));
    let m_big = be_to_biguint(core::slice::from_raw_parts(m, len));
    if m_big.is_zero() {
        return CX_INVALID_PARAMETER;
    }
    let result = a_big.modpow(&e_big, &m_big);
    let result_bytes = biguint_to_be_padded(&result, len);
    core::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, len);
    CX_OK
}

/// cx_math_invprimem_no_throw — r = a^(-1) mod m (Fermat's little theorem; m must be prime).
#[no_mangle]
pub unsafe extern "C" fn cx_math_invprimem_no_throw(r: *mut u8, a: *const u8, m: *const u8, len: u32) -> u32 {
    if r.is_null() || a.is_null() || m.is_null() || len == 0 {
        return CX_INVALID_PARAMETER;
    }
    let len = len as usize;
    let a_big = be_to_biguint(core::slice::from_raw_parts(a, len));
    let m_big = be_to_biguint(core::slice::from_raw_parts(m, len));
    if m_big.is_zero() || a_big.is_zero() {
        return CX_INVALID_PARAMETER;
    }
    let exp = &m_big - BigUint::from(2u32);
    let result = a_big.modpow(&exp, &m_big);
    let result_bytes = biguint_to_be_padded(&result, len);
    core::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, len);
    CX_OK
}

/// cx_math_cmp_no_throw — compare a and b (big-endian).
/// Sets *diff < 0 if a < b, 0 if equal, > 0 if a > b.
#[no_mangle]
pub unsafe extern "C" fn cx_math_cmp_no_throw(
    a: *const u8,
    b: *const u8,
    length: u32,
    diff: *mut i32,
) -> u32 {
    if a.is_null() || b.is_null() || diff.is_null() || length == 0 {
        return CX_INVALID_PARAMETER;
    }
    let a_big = be_to_biguint(core::slice::from_raw_parts(a, length as usize));
    let b_big = be_to_biguint(core::slice::from_raw_parts(b, length as usize));
    *diff = match a_big.cmp(&b_big) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    };
    CX_OK
}

/// cx_math_sub_no_throw — r = a - b (plain subtraction). Returns CX_CARRY if a < b.
#[no_mangle]
pub unsafe extern "C" fn cx_math_sub_no_throw(r: *mut u8, a: *const u8, b: *const u8, len: u32) -> u32 {
    if r.is_null() || a.is_null() || b.is_null() || len == 0 {
        return CX_INVALID_PARAMETER;
    }
    let len = len as usize;
    let a_big = be_to_biguint(core::slice::from_raw_parts(a, len));
    let b_big = be_to_biguint(core::slice::from_raw_parts(b, len));
    let (result, carry) = if a_big >= b_big {
        (a_big - &b_big, false)
    } else {
        let modulus = BigUint::one() << (len * 8);
        (modulus + a_big - &b_big, true)
    };
    let result_bytes = biguint_to_be_padded(&result, len);
    core::ptr::copy_nonoverlapping(result_bytes.as_ptr(), r, len);
    if carry {
        CX_CARRY
    } else {
        CX_OK
    }
}

// ============================================================================
// Ed25519 Point Operations
// ============================================================================

/// Convert SDK big-endian uncompressed point (04 || x_BE(32) || y_BE(32))
/// to a curve25519-dalek EdwardsPoint.
fn be_uncompressed_to_edwards(p: &[u8]) -> Option<EdwardsPoint> {
    if p.len() < 65 || p[0] != 0x04 {
        return None;
    }
    let x_be = &p[1..33];
    let y_be = &p[33..65];
    // Sign of x = low bit of x value. In big-endian, the LSB is x_be[31].
    let sign = x_be[31] & 1;
    // Reverse y to little-endian and set sign bit in bit 255 (standard Ed25519).
    let mut compressed_le = [0u8; 32];
    for i in 0..32 {
        compressed_le[i] = y_be[31 - i];
    }
    compressed_le[31] |= sign << 7;
    CompressedEdwardsY(compressed_le).decompress()
}

/// Recover the x-coordinate on Ed25519 given y and the sign of x.
/// Uses the curve equation -x² + y² = 1 + d·x²·y².
fn recover_ed25519_x(y: &BigUint, sign: u8, p: &BigUint, d: &BigUint) -> BigUint {
    let one = BigUint::one();
    let y2 = (y * y) % p;
    // u = y² - 1 mod p (add p to prevent BigUint underflow)
    let u = (p + &y2 - &one) % p;
    // v = d·y² + 1 mod p
    let v = (d * &y2 + &one) % p;
    // x² = u · v⁻¹ mod p
    let v_inv = v.modpow(&(p - BigUint::from(2u32)), p);
    let x2 = (&u * &v_inv) % p;

    if x2.is_zero() {
        return BigUint::zero();
    }

    // x = (x²)^((p+3)/8) mod p
    let exp = (p + BigUint::from(3u32)) >> 3;
    let mut x = x2.modpow(&exp, p);

    // Verify: if x² ≢ x² (mod p), multiply by sqrt(-1)
    if (&x * &x) % p != x2 {
        x = (&x * &*ED25519_SQRT_M1) % p;
    }

    // Adjust sign: low bit of x must equal `sign`
    let x_le = x.to_bytes_le();
    let x_low_bit = if x_le.is_empty() { 0 } else { x_le[0] & 1 };
    if x_low_bit != sign {
        x = p - &x;
    }

    x
}

/// Convert curve25519-dalek EdwardsPoint to SDK big-endian uncompressed format
/// (04 || x_BE(32) || y_BE(32)).
fn edwards_to_be_uncompressed(point: &EdwardsPoint) -> [u8; 65] {
    let compressed = point.compress();
    let s = compressed.to_bytes(); // y_LE with sign in bit 255
    let sign = (s[31] >> 7) & 1;
    let mut y_le = s;
    y_le[31] &= 0x7F; // Clear sign bit

    let y = BigUint::from_bytes_le(&y_le);
    let x = recover_ed25519_x(&y, sign, &ED25519_P, &ED25519_D);

    let x_be = biguint_to_be_padded(&x, 32);
    let mut y_be = [0u8; 32];
    for i in 0..32 {
        y_be[i] = y_le[31 - i];
    }

    let mut result = [0u8; 65];
    result[0] = 0x04;
    result[1..33].copy_from_slice(&x_be);
    result[33..65].copy_from_slice(&y_be);
    result
}

/// Convert a big-endian scalar to a curve25519-dalek Scalar.
fn be_scalar_to_dalek(k: &[u8]) -> DalekScalar {
    let mut le = [0u8; 32];
    for (i, &b) in k.iter().rev().enumerate() {
        if i < 32 {
            le[i] = b;
        }
    }
    DalekScalar::from_bytes_mod_order(le)
}

/// cx_ecfp_scalar_mult_no_throw(cx_curve_t curve, uint8_t *P,
///     const uint8_t *k, size_t k_len)
///
/// Ed25519 scalar multiplication: P = k * P (in-place).
/// P is in SDK uncompressed format (04 || x_BE(32) || y_BE(32)).
/// k is a big-endian scalar.
#[no_mangle]
pub unsafe extern "C" fn cx_ecfp_scalar_mult_no_throw(
    curve: u32,
    p: *mut u8,
    k: *const u8,
    k_len: u32,
) -> u32 {
    if p.is_null() || k.is_null() || k_len == 0 {
        return CX_INVALID_PARAMETER;
    }
    if curve != CX_CURVE_ED25519 {
        log::warn!("cx_ecfp_scalar_mult_no_throw: unsupported curve 0x{:02x}", curve);
        return CX_INVALID_PARAMETER;
    }

    let p_buf = core::slice::from_raw_parts(p, 65);
    let point = match be_uncompressed_to_edwards(p_buf) {
        Some(pt) => pt,
        None => {
            log::warn!("cx_ecfp_scalar_mult_no_throw: invalid point");
            return CX_INVALID_PARAMETER;
        }
    };

    let k_buf = core::slice::from_raw_parts(k, k_len as usize);
    let scalar = be_scalar_to_dalek(k_buf);
    let result = &point * &scalar;
    let out = edwards_to_be_uncompressed(&result);
    core::ptr::copy_nonoverlapping(out.as_ptr(), p, 65);

    log::debug!("cx_ecfp_scalar_mult_no_throw: Ed25519 scalar mult OK");
    CX_OK
}

/// cx_ecfp_add_point_no_throw(cx_curve_t curve, uint8_t *R,
///     const uint8_t *P, const uint8_t *Q)
///
/// Ed25519 point addition: R = P + Q.
/// All points in SDK uncompressed format (04 || x_BE(32) || y_BE(32)).
#[no_mangle]
pub unsafe extern "C" fn cx_ecfp_add_point_no_throw(
    curve: u32,
    r: *mut u8,
    p: *const u8,
    q: *const u8,
) -> u32 {
    if r.is_null() || p.is_null() || q.is_null() {
        return CX_INVALID_PARAMETER;
    }
    if curve != CX_CURVE_ED25519 {
        log::warn!("cx_ecfp_add_point_no_throw: unsupported curve 0x{:02x}", curve);
        return CX_INVALID_PARAMETER;
    }

    let p_buf = core::slice::from_raw_parts(p, 65);
    let q_buf = core::slice::from_raw_parts(q, 65);

    let p_point = match be_uncompressed_to_edwards(p_buf) {
        Some(pt) => pt,
        None => {
            log::warn!("cx_ecfp_add_point_no_throw: invalid point P");
            return CX_INVALID_PARAMETER;
        }
    };
    let q_point = match be_uncompressed_to_edwards(q_buf) {
        Some(pt) => pt,
        None => {
            log::warn!("cx_ecfp_add_point_no_throw: invalid point Q");
            return CX_INVALID_PARAMETER;
        }
    };

    let result = &p_point + &q_point;
    let out = edwards_to_be_uncompressed(&result);
    core::ptr::copy_nonoverlapping(out.as_ptr(), r, 65);
    CX_OK
}

/// cx_edwards_compress_point_no_throw(cx_curve_t curve, uint8_t *P, size_t P_len)
///
/// Compress an Ed25519 point from uncompressed (04 || x_BE || y_BE) to
/// compressed (02 || y_LE_with_sign). The compressed form is standard Ed25519
/// (y in little-endian, sign of x in bit 255).
#[no_mangle]
pub unsafe extern "C" fn cx_edwards_compress_point_no_throw(curve: u32, p: *mut u8, _p_len: u32) -> u32 {
    if p.is_null() {
        return CX_INVALID_PARAMETER;
    }
    if curve != CX_CURVE_ED25519 {
        log::warn!("cx_edwards_compress_point_no_throw: unsupported curve 0x{:02x}", curve);
        return CX_INVALID_PARAMETER;
    }

    let p_buf = core::slice::from_raw_parts(p, 65);
    let point = match be_uncompressed_to_edwards(p_buf) {
        Some(pt) => pt,
        None => {
            log::warn!("cx_edwards_compress_point_no_throw: invalid point");
            return CX_INVALID_PARAMETER;
        }
    };

    // Compress to standard Ed25519 format (y_LE with sign in bit 255)
    let compressed = point.compress().to_bytes();

    // Write compressed format: 02 || compressed_LE(32)
    *p = 0x02;
    core::ptr::copy_nonoverlapping(compressed.as_ptr(), p.add(1), 32);
    // Copy to second half of buffer for SDK compatibility
    core::ptr::copy_nonoverlapping(compressed.as_ptr(), p.add(33), 32);
    CX_OK
}

/// cx_edwards_decompress_point_no_throw(cx_curve_t curve, uint8_t *P, size_t P_len)
///
/// Decompress an Ed25519 point from compressed (02 || y_LE_with_sign) to
/// uncompressed (04 || x_BE || y_BE).
#[no_mangle]
pub unsafe extern "C" fn cx_edwards_decompress_point_no_throw(curve: u32, p: *mut u8, _p_len: u32) -> u32 {
    if p.is_null() {
        return CX_INVALID_PARAMETER;
    }
    if curve != CX_CURVE_ED25519 {
        log::warn!("cx_edwards_decompress_point_no_throw: unsupported curve 0x{:02x}", curve);
        return CX_INVALID_PARAMETER;
    }

    // Read compressed: P[1..33] = y_LE(32) with sign in bit 255
    let mut compressed = [0u8; 32];
    core::ptr::copy_nonoverlapping(p.add(1), compressed.as_mut_ptr(), 32);

    let point = match CompressedEdwardsY(compressed).decompress() {
        Some(pt) => pt,
        None => {
            log::warn!("cx_edwards_decompress_point_no_throw: invalid compressed point");
            return CX_INVALID_PARAMETER;
        }
    };

    // Convert to SDK uncompressed format: 04 || x_BE(32) || y_BE(32)
    let uncompressed = edwards_to_be_uncompressed(&point);
    core::ptr::copy_nonoverlapping(uncompressed.as_ptr(), p, 65);
    CX_OK
}

// ============================================================================
// Additional SDK crypto helpers
// ============================================================================

/// cx_ecfp_decode_sig_der — DER signature decoder (stub: not called at runtime for home screen)
#[cfg(not(keyos))]
#[no_mangle]
pub unsafe extern "C" fn cx_ecfp_decode_sig_der(
    _input: *const u8,
    _input_len: u32,
    _max_size: u32,
    _r: *mut *const u8,
    _r_len: *mut u32,
    _s: *mut *const u8,
    _s_len: *mut u32,
) -> u32 {
    log::warn!("cx_ecfp_decode_sig_der: stub called");
    CX_INTERNAL_ERROR
}

/// cx_math_mult_no_throw — big number multiply: r[2*len] = a[len] * b[len] (big-endian)
#[no_mangle]
pub unsafe extern "C" fn cx_math_mult_no_throw(r: *mut u8, a: *const u8, b: *const u8, len: u32) -> u32 {
    let len = len as usize;
    if len == 0 || r.is_null() || a.is_null() || b.is_null() {
        return CX_INTERNAL_ERROR;
    }
    let a_slice = core::slice::from_raw_parts(a, len);
    let b_slice = core::slice::from_raw_parts(b, len);
    let r_slice = core::slice::from_raw_parts_mut(r, 2 * len);

    r_slice.fill(0);

    for i in (0..len).rev() {
        let mut carry: u16 = 0;
        for j in (0..len).rev() {
            let pos = i + j + 1;
            let prod = (a_slice[i] as u16) * (b_slice[j] as u16) + (r_slice[pos] as u16) + carry;
            r_slice[pos] = prod as u8;
            carry = prod >> 8;
        }
        r_slice[i] = r_slice[i].wrapping_add(carry as u8);
    }

    CX_OK
}

/// cx_x25519 — X25519 ECDH (stub)
#[cfg(not(keyos))]
#[no_mangle]
pub unsafe extern "C" fn cx_x25519(_u: *mut u8, _k: *const u8, _point_len: u32) -> u32 {
    log::warn!("cx_x25519: stub called");
    CX_INTERNAL_ERROR
}

// ============================================================================
// AES Operations
// ============================================================================

/// cx_aes_init_key_no_throw(const uint8_t *raw_key, size_t key_len, cx_aes_key_t *key)
///
/// Initialize an AES key structure. cx_aes_key_t layout: { u32 size; u8 keys[32]; }
#[no_mangle]
pub unsafe extern "C" fn cx_aes_init_key_no_throw(raw_key: *const u8, key_len: u32, key: *mut u8) -> u32 {
    if key.is_null() {
        return CX_INVALID_PARAMETER;
    }

    // Zero-initialize: sizeof(cx_aes_key_t) = 4 + 32 = 36
    core::ptr::write_bytes(key, 0, 36);

    let key_len_usize = key_len as usize;
    if key_len_usize != 16 && key_len_usize != 24 && key_len_usize != 32 {
        return CX_INVALID_PARAMETER;
    }

    // Write size field (offset 0)
    let key_u32 = key as *mut u32;
    *key_u32 = key_len;

    // Copy raw key to keys field (offset 4)
    if !raw_key.is_null() {
        core::ptr::copy_nonoverlapping(raw_key, key.add(4), key_len_usize);
    }

    log::debug!("cx_aes_init_key_no_throw: key_len={}", key_len);
    CX_OK
}

/// cx_aes_no_throw — AES encrypt/decrypt. Supports ECB and CBC modes with no padding.
///
/// cx_aes_key_t layout: { u32 size; u8 keys[32]; }
/// Mode flags: CX_ENCRYPT (0x100), CX_CHAIN_CBC (0x800), CX_LAST (0x01), CX_PAD_NONE (0x00).
#[no_mangle]
pub unsafe extern "C" fn cx_aes_no_throw(
    key: *const u8,
    mode: u32,
    input: *const u8,
    in_len: u32,
    output: *mut u8,
    out_len: *mut u32,
) -> u32 {
    if key.is_null() || input.is_null() || output.is_null() {
        return CX_INVALID_PARAMETER;
    }

    let key_u32 = key as *const u32;
    let key_size = *key_u32 as usize;
    if key_size != 16 {
        log::warn!("cx_aes_no_throw: only AES-128 supported, got key_size={}", key_size);
        return CX_INVALID_PARAMETER;
    }
    let key_data = core::slice::from_raw_parts(key.add(4), 16);
    let in_data = core::slice::from_raw_parts(input, in_len as usize);
    let out_data = core::slice::from_raw_parts_mut(output, in_len as usize);

    let cipher = Aes128::new(aes::cipher::generic_array::GenericArray::from_slice(key_data));
    let encrypt = (mode & CX_AES_ENCRYPT) != 0;
    let cbc = (mode & CX_AES_CHAIN_CBC) != 0;

    // The cx_aes_no_throw reference is defined as cx_aes_iv_no_throw with a
    // sixteen-zero initial IV.
    let mut prev = [0u8; 16];

    for chunk_start in (0..in_len as usize).step_by(16) {
        if chunk_start + 16 > in_len as usize {
            break; // Skip partial blocks (shouldn't happen with PAD_NONE)
        }

        let mut block = aes::cipher::generic_array::GenericArray::clone_from_slice(
            &in_data[chunk_start..chunk_start + 16],
        );

        if encrypt {
            if cbc {
                for i in 0..16 {
                    block[i] ^= prev[i];
                }
            }
            cipher.encrypt_block(&mut block);
            if cbc {
                prev.copy_from_slice(&block);
            }
        } else {
            let mut saved = [0u8; 16];
            if cbc {
                saved.copy_from_slice(&in_data[chunk_start..chunk_start + 16]);
            }
            cipher.decrypt_block(&mut block);
            if cbc {
                for i in 0..16 {
                    block[i] ^= prev[i];
                }
                prev = saved;
            }
        }

        out_data[chunk_start..chunk_start + 16].copy_from_slice(&block);
    }

    if !out_len.is_null() {
        *out_len = in_len;
    }

    log::debug!("cx_aes_no_throw: mode=0x{:x}, in_len={}, encrypt={}", mode, in_len, encrypt);
    CX_OK
}
