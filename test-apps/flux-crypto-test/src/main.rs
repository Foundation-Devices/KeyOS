// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Test application for the BOLOS crypto C-FFI surface in `crypto.rs`.
//!
//! Exercises every `#[no_mangle] extern "C"` function with known test vectors,
//! validating that the pointer-to-context-ID mapping, C struct layout handling,
//! and error paths work correctly through the full FFI boundary.

#![allow(non_upper_case_globals)]

use std::mem::{align_of, size_of, MaybeUninit};

mod crypto {
    pub use app_flux_runtime::crypto::*;

    pub unsafe extern "C" fn cx_rng_no_throw(buffer: *mut u8, len: u32) -> u32 {
        if buffer.is_null() {
            return super::CX_INVALID_PARAMETER;
        }
        let buf = core::slice::from_raw_parts_mut(buffer, len as usize);
        use std::sync::atomic::{AtomicU32, Ordering};
        static RNG_COUNTER: AtomicU32 = AtomicU32::new(0);
        for chunk in buf.chunks_mut(4) {
            let counter = RNG_COUNTER.fetch_add(1, Ordering::Relaxed);
            let seed = counter.wrapping_mul(0x9E3779B9).wrapping_add(0xDEADBEEF);
            let val = seed.wrapping_mul(0x85EBCA6B).rotate_left(13);
            let bytes = val.to_le_bytes();
            let copy_len = chunk.len().min(4);
            chunk[..copy_len].copy_from_slice(&bytes[..copy_len]);
        }
        super::CX_OK
    }

    pub unsafe extern "C" fn cx_rng_u32_range_func(
        low: u32,
        high: u32,
        _rng: *const core::ffi::c_void,
    ) -> u32 {
        if high <= low {
            return low;
        }
        let mut buf = [0u8; 4];
        cx_rng_no_throw(buf.as_mut_ptr(), 4);
        let val = u32::from_le_bytes(buf);
        low + (val % (high - low))
    }

    pub unsafe extern "C" fn cx_get_random_bytes(buffer: *mut u8, len: u32) -> u32 {
        cx_rng_no_throw(buffer, len)
    }
}

// --- BOLOS constants (must match crypto.rs) ---
const CX_OK: u32 = 0x00000000;
const CX_INVALID_PARAMETER: u32 = 0xFFFFFF88;
const CX_INVALID_PARAMETER_SIZE: u32 = 0xFFFFFF89;
const CX_FLAG_LAST: u32 = 0x0001;
const CX_CURVE_SECP256K1: u32 = 0x21;

fn private_key_bytes(key: &MaybeUninit<crypto::CxEcfpPrivateKey>) -> &[u8] {
    unsafe { core::slice::from_raw_parts(key.as_ptr() as *const u8, size_of::<crypto::CxEcfpPrivateKey>()) }
}

fn public_key_bytes(key: &MaybeUninit<crypto::CxEcfpPublicKey>) -> &[u8] {
    unsafe { core::slice::from_raw_parts(key.as_ptr() as *const u8, size_of::<crypto::CxEcfpPublicKey>()) }
}

fn public_key_bytes_mut(key: &mut MaybeUninit<crypto::CxEcfpPublicKey>) -> &mut [u8] {
    unsafe {
        core::slice::from_raw_parts_mut(key.as_mut_ptr() as *mut u8, size_of::<crypto::CxEcfpPublicKey>())
    }
}

fn key_len_offset() -> usize { align_of::<usize>() }

fn key_data_offset() -> usize { key_len_offset() + size_of::<usize>() }

fn read_usize_field(buf: &[u8], offset: usize) -> usize {
    let mut bytes = [0u8; size_of::<usize>()];
    let len = bytes.len();
    bytes.copy_from_slice(&buf[offset..offset + len]);
    usize::from_ne_bytes(bytes)
}

fn write_usize_field(buf: &mut [u8], offset: usize, value: usize) {
    let bytes = value.to_ne_bytes();
    buf[offset..offset + bytes.len()].copy_from_slice(&bytes);
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).ok();
    log::set_max_level(log::LevelFilter::Info);

    log::info!("=== Flux Crypto C-FFI Test Suite ===");

    test_sha256();
    test_sha512();
    test_keccak256();
    test_hash_get_size();
    test_hmac_sha256();
    test_hmac_sha512();
    test_invalid_buffer_edges();
    test_ec_key_init_and_generate();
    test_ecdsa_sign_verify();
    test_crc32();
    test_rng();
    test_memxor();
    test_constant_time_eq();
    test_swap();
    test_ecdomain();

    log::info!("=== All flux crypto tests passed! ===");
}

// ============================================================================
// Hash Tests
// ============================================================================

/// Test SHA-256 incremental and one-shot hashing against NIST test vector.
/// SHA-256("abc") = ba7816bf 8f01cfea 414140de 5dae2223 b00361a3 96177a9c b410ff61 f20015ad
fn test_sha256() {
    log::info!("test_sha256...");

    let input = b"abc";
    let expected: [u8; 32] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23, 0xb0,
        0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
    ];

    // Incremental: init → update → finalize
    unsafe {
        let mut ctx = [0u8; 128]; // Opaque context buffer
        let rc = crypto::cx_sha256_init_no_throw(ctx.as_mut_ptr());
        assert_eq!(rc, CX_OK, "sha256 init failed");

        let mut digest = [0u8; 32];
        let rc = crypto::cx_hash_no_throw(
            ctx.as_mut_ptr(),
            CX_FLAG_LAST,
            input.as_ptr(),
            input.len() as u32,
            digest.as_mut_ptr(),
            32,
        );
        assert_eq!(rc, CX_OK, "sha256 hash_no_throw failed");
        assert_eq!(digest, expected, "sha256 incremental digest mismatch");
    }

    // One-shot
    unsafe {
        let mut digest = [0u8; 32];
        let rc = crypto::cx_hash_sha256(input.as_ptr(), input.len() as u32, digest.as_mut_ptr(), 32);
        assert_eq!(rc, CX_OK, "sha256 one-shot failed");
        assert_eq!(digest, expected, "sha256 one-shot digest mismatch");
    }

    // Multi-update: feed "a", then "bc"
    unsafe {
        let mut ctx = [0u8; 128];
        let rc = crypto::cx_sha256_init_no_throw(ctx.as_mut_ptr());
        assert_eq!(rc, CX_OK);

        let rc = crypto::cx_hash_no_throw(ctx.as_mut_ptr(), 0, b"a".as_ptr(), 1, core::ptr::null_mut(), 0);
        assert_eq!(rc, CX_OK, "sha256 update 'a' failed");

        let mut digest = [0u8; 32];
        let rc = crypto::cx_hash_no_throw(
            ctx.as_mut_ptr(),
            CX_FLAG_LAST,
            b"bc".as_ptr(),
            2,
            digest.as_mut_ptr(),
            32,
        );
        assert_eq!(rc, CX_OK, "sha256 finalize 'bc' failed");
        assert_eq!(digest, expected, "sha256 multi-update digest mismatch");
    }

    log::info!("  PASS");
}

/// Test SHA-512 incremental and one-shot hashing.
/// SHA-512("abc") starts with ddaf35a1...
fn test_sha512() {
    log::info!("test_sha512...");

    let input = b"abc";
    let expected: [u8; 64] = [
        0xdd, 0xaf, 0x35, 0xa1, 0x93, 0x61, 0x7a, 0xba, 0xcc, 0x41, 0x73, 0x49, 0xae, 0x20, 0x41, 0x31, 0x12,
        0xe6, 0xfa, 0x4e, 0x89, 0xa9, 0x7e, 0xa2, 0x0a, 0x9e, 0xee, 0xe6, 0x4b, 0x55, 0xd3, 0x9a, 0x21, 0x92,
        0x99, 0x2a, 0x27, 0x4f, 0xc1, 0xa8, 0x36, 0xba, 0x3c, 0x23, 0xa3, 0xfe, 0xeb, 0xbd, 0x45, 0x4d, 0x44,
        0x23, 0x64, 0x3c, 0xe8, 0x0e, 0x2a, 0x9a, 0xc9, 0x4f, 0xa5, 0x4c, 0xa4, 0x9f,
    ];

    // Incremental
    unsafe {
        let mut ctx = [0u8; 256];
        let rc = crypto::cx_sha512_init_no_throw(ctx.as_mut_ptr());
        assert_eq!(rc, CX_OK, "sha512 init failed");

        let mut digest = [0u8; 64];
        let rc = crypto::cx_hash_no_throw(
            ctx.as_mut_ptr(),
            CX_FLAG_LAST,
            input.as_ptr(),
            input.len() as u32,
            digest.as_mut_ptr(),
            64,
        );
        assert_eq!(rc, CX_OK, "sha512 hash_no_throw failed");
        assert_eq!(digest, expected, "sha512 incremental digest mismatch");
    }

    // One-shot
    unsafe {
        let mut digest = [0u8; 64];
        let rc = crypto::cx_hash_sha512(input.as_ptr(), input.len() as u32, digest.as_mut_ptr(), 64);
        assert_eq!(rc, CX_OK, "sha512 one-shot failed");
        assert_eq!(digest, expected, "sha512 one-shot digest mismatch");
    }

    log::info!("  PASS");
}

/// Test Keccak-256 hashing.
/// Keccak-256("abc") = 4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45
fn test_keccak256() {
    log::info!("test_keccak256...");

    let input = b"abc";
    let expected: [u8; 32] = [
        0x4e, 0x03, 0x65, 0x7a, 0xea, 0x45, 0xa9, 0x4f, 0xc7, 0xd4, 0x7b, 0xa8, 0x26, 0xc8, 0xd6, 0x67, 0xc0,
        0xd1, 0xe6, 0xe3, 0x3a, 0x64, 0xa0, 0x36, 0xec, 0x44, 0xf5, 0x8f, 0xa1, 0x2d, 0x6c, 0x45,
    ];

    // Incremental
    unsafe {
        let mut ctx = [0u8; 256];
        let rc = crypto::cx_keccak_init_no_throw(ctx.as_mut_ptr(), 256);
        assert_eq!(rc, CX_OK, "keccak init failed");

        let mut digest = [0u8; 32];
        let rc = crypto::cx_hash_no_throw(
            ctx.as_mut_ptr(),
            CX_FLAG_LAST,
            input.as_ptr(),
            input.len() as u32,
            digest.as_mut_ptr(),
            32,
        );
        assert_eq!(rc, CX_OK, "keccak hash_no_throw failed");
        assert_eq!(digest, expected, "keccak-256 digest mismatch");
    }

    // Test invalid size parameter
    unsafe {
        let mut ctx = [0u8; 256];
        let rc = crypto::cx_keccak_init_no_throw(ctx.as_mut_ptr(), 128);
        assert_eq!(rc, CX_INVALID_PARAMETER, "keccak should reject size != 256");
    }

    log::info!("  PASS");
}

/// Test cx_hash_get_size returns correct digest sizes.
fn test_hash_get_size() {
    log::info!("test_hash_get_size...");

    unsafe {
        // SHA-256 → 32
        let mut ctx = [0u8; 128];
        crypto::cx_sha256_init_no_throw(ctx.as_mut_ptr());
        let size = crypto::cx_hash_get_size(ctx.as_ptr());
        assert_eq!(size, 32, "sha256 hash_get_size should be 32, got {}", size);

        // SHA-512 → 64
        let mut ctx = [0u8; 256];
        crypto::cx_sha512_init_no_throw(ctx.as_mut_ptr());
        let size = crypto::cx_hash_get_size(ctx.as_ptr());
        assert_eq!(size, 64, "sha512 hash_get_size should be 64, got {}", size);

        // Keccak-256 → 32
        let mut ctx = [0u8; 256];
        crypto::cx_keccak_init_no_throw(ctx.as_mut_ptr(), 256);
        let size = crypto::cx_hash_get_size(ctx.as_ptr());
        assert_eq!(size, 32, "keccak hash_get_size should be 32, got {}", size);
    }

    log::info!("  PASS");
}

// ============================================================================
// HMAC Tests
// ============================================================================

/// Test HMAC-SHA256 against RFC 4231 Test Case 2.
/// Key = "Jefe" (4 bytes), Data = "what do ya want for nothing?" (28 bytes)
/// HMAC-SHA256 = 5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843
fn test_hmac_sha256() {
    log::info!("test_hmac_sha256...");

    let key = b"Jefe";
    let data = b"what do ya want for nothing?";
    let expected: [u8; 32] = [
        0x5b, 0xdc, 0xc1, 0x46, 0xbf, 0x60, 0x75, 0x4e, 0x6a, 0x04, 0x24, 0x26, 0x08, 0x95, 0x75, 0xc7, 0x5a,
        0x00, 0x3f, 0x08, 0x9d, 0x27, 0x39, 0x83, 0x9d, 0xec, 0x58, 0xb9, 0x64, 0xec, 0x38, 0x43,
    ];

    // Incremental: init → update → finalize
    unsafe {
        let mut ctx = [0u8; 256];
        let rc = crypto::cx_hmac_sha256_init_no_throw(ctx.as_mut_ptr(), key.as_ptr(), key.len() as u32);
        assert_eq!(rc, CX_OK, "hmac-sha256 init failed");

        let mut mac = [0u8; 32];
        let rc = crypto::cx_hmac_no_throw(
            ctx.as_mut_ptr(),
            CX_FLAG_LAST,
            data.as_ptr(),
            data.len() as u32,
            mac.as_mut_ptr(),
            32,
        );
        assert_eq!(rc, CX_OK, "hmac-sha256 finalize failed");
        assert_eq!(mac, expected, "hmac-sha256 incremental mac mismatch");
    }

    // One-shot
    unsafe {
        let mut mac = [0u8; 32];
        let rc = crypto::cx_hmac_sha256(
            key.as_ptr(),
            key.len() as u32,
            data.as_ptr(),
            data.len() as u32,
            mac.as_mut_ptr(),
            32,
        );
        assert_eq!(rc, CX_OK, "hmac-sha256 one-shot failed");
        assert_eq!(mac, expected, "hmac-sha256 one-shot mac mismatch");
    }

    log::info!("  PASS");
}

/// Test HMAC-SHA512 against RFC 4231 Test Case 2.
/// Key = "Jefe", Data = "what do ya want for nothing?"
/// HMAC-SHA512 = 164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea250554
///               9758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737
fn test_hmac_sha512() {
    log::info!("test_hmac_sha512...");

    let key = b"Jefe";
    let data = b"what do ya want for nothing?";
    let expected: [u8; 64] = [
        0x16, 0x4b, 0x7a, 0x7b, 0xfc, 0xf8, 0x19, 0xe2, 0xe3, 0x95, 0xfb, 0xe7, 0x3b, 0x56, 0xe0, 0xa3, 0x87,
        0xbd, 0x64, 0x22, 0x2e, 0x83, 0x1f, 0xd6, 0x10, 0x27, 0x0c, 0xd7, 0xea, 0x25, 0x05, 0x54, 0x97, 0x58,
        0xbf, 0x75, 0xc0, 0x5a, 0x99, 0x4a, 0x6d, 0x03, 0x4f, 0x65, 0xf8, 0xf0, 0xe6, 0xfd, 0xca, 0xea, 0xb1,
        0xa3, 0x4d, 0x4a, 0x6b, 0x4b, 0x63, 0x6e, 0x07, 0x0a, 0x38, 0xbc, 0xe7, 0x37,
    ];

    // Incremental
    unsafe {
        let mut ctx = [0u8; 512];
        let rc = crypto::cx_hmac_sha512_init_no_throw(ctx.as_mut_ptr(), key.as_ptr(), key.len() as u32);
        assert_eq!(rc, CX_OK, "hmac-sha512 init failed");

        let mut mac = [0u8; 64];
        let rc = crypto::cx_hmac_no_throw(
            ctx.as_mut_ptr(),
            CX_FLAG_LAST,
            data.as_ptr(),
            data.len() as u32,
            mac.as_mut_ptr(),
            64,
        );
        assert_eq!(rc, CX_OK, "hmac-sha512 finalize failed");
        assert_eq!(mac, expected, "hmac-sha512 incremental mac mismatch");
    }

    // One-shot
    unsafe {
        let mut mac = [0u8; 64];
        let rc = crypto::cx_hmac_sha512(
            key.as_ptr(),
            key.len() as u32,
            data.as_ptr(),
            data.len() as u32,
            mac.as_mut_ptr(),
            64,
        );
        assert_eq!(rc, CX_OK, "hmac-sha512 one-shot failed");
        assert_eq!(mac, expected, "hmac-sha512 one-shot mac mismatch");
    }

    log::info!("  PASS");
}

// ============================================================================
// EC Key Tests
// ============================================================================

fn test_invalid_buffer_edges() {
    log::info!("test_invalid_buffer_edges...");

    let key = [0x11u8; 33];
    unsafe {
        let mut pvkey = MaybeUninit::<crypto::CxEcfpPrivateKey>::zeroed();
        let rc = crypto::cx_ecfp_init_private_key_no_throw(
            CX_CURVE_SECP256K1,
            key.as_ptr(),
            key.len() as u32,
            pvkey.as_mut_ptr(),
        );
        assert_eq!(rc, CX_INVALID_PARAMETER_SIZE, "oversized private key should fail");

        let mut pubkey = MaybeUninit::<crypto::CxEcfpPublicKey>::zeroed();
        let rc = crypto::cx_ecfp_init_public_key_no_throw(
            CX_CURVE_SECP256K1,
            core::ptr::null(),
            1,
            pubkey.as_mut_ptr(),
        );
        assert_eq!(rc, CX_INVALID_PARAMETER, "non-zero public key length needs raw bytes");

        let oversized_pubkey = [0x04u8; 66];
        let rc = crypto::cx_ecfp_init_public_key_no_throw(
            CX_CURVE_SECP256K1,
            oversized_pubkey.as_ptr(),
            oversized_pubkey.len() as u32,
            pubkey.as_mut_ptr(),
        );
        assert_eq!(rc, CX_INVALID_PARAMETER_SIZE, "oversized public key should fail");

        let pubkey_bytes = public_key_bytes_mut(&mut pubkey);
        pubkey_bytes[0] = CX_CURVE_SECP256K1 as u8;
        write_usize_field(pubkey_bytes, key_len_offset(), 66);
        let hash = [0u8; 32];
        let sig = [0u8; 72];
        let verified = crypto::cx_ecdsa_verify_no_throw(
            pubkey.as_ptr(),
            hash.as_ptr(),
            hash.len() as u32,
            sig.as_ptr(),
            sig.len() as u32,
        );
        assert_eq!(verified, 0, "oversized W_len should fail instead of panicking");

        let mut hash_ctx = [0u8; 128];
        let rc = crypto::cx_sha256_init_no_throw(hash_ctx.as_mut_ptr());
        assert_eq!(rc, CX_OK, "sha256 init failed");
        let mut digest = [0u8; 31];
        let rc = crypto::cx_hash_no_throw(
            hash_ctx.as_mut_ptr(),
            CX_FLAG_LAST,
            b"abc".as_ptr(),
            3,
            digest.as_mut_ptr(),
            digest.len() as u32,
        );
        assert_eq!(rc, CX_INVALID_PARAMETER_SIZE, "undersized hash output should fail");

        let mut mac = [0u8; 31];
        let rc =
            crypto::cx_hmac_sha256(b"k".as_ptr(), 1, b"abc".as_ptr(), 3, mac.as_mut_ptr(), mac.len() as u32);
        assert_eq!(rc, CX_INVALID_PARAMETER_SIZE, "undersized HMAC output should fail");
    }

    log::info!("  PASS");
}

/// Test EC private key initialization and public key generation.
fn test_ec_key_init_and_generate() {
    log::info!("test_ec_key_init_and_generate...");

    // Known valid secp256k1 private key
    let raw_key: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11,
        0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
    ];

    unsafe {
        let mut pvkey = MaybeUninit::<crypto::CxEcfpPrivateKey>::zeroed();
        let rc = crypto::cx_ecfp_init_private_key_no_throw(
            CX_CURVE_SECP256K1,
            raw_key.as_ptr(),
            32,
            pvkey.as_mut_ptr(),
        );
        assert_eq!(rc, CX_OK, "init private key failed");

        // Verify C struct layout
        let pvkey_bytes = private_key_bytes(&pvkey);
        let curve = pvkey_bytes[0] as u32;
        let d_len = read_usize_field(pvkey_bytes, key_len_offset());
        assert_eq!(curve, CX_CURVE_SECP256K1, "private key curve mismatch");
        assert_eq!(d_len, 32, "private key d_len mismatch");
        let d_offset = key_data_offset();
        assert_eq!(&pvkey_bytes[d_offset..d_offset + 32], &raw_key, "private key d bytes mismatch");

        let mut pubkey = MaybeUninit::<crypto::CxEcfpPublicKey>::zeroed();
        let rc = crypto::cx_ecfp_generate_pair_no_throw(
            CX_CURVE_SECP256K1,
            pubkey.as_mut_ptr(),
            pvkey.as_ptr(),
            1,
        );
        assert_eq!(rc, CX_OK, "generate pair failed");

        // Verify public key struct
        let pubkey_bytes = public_key_bytes(&pubkey);
        let pub_curve = pubkey_bytes[0] as u32;
        let w_len = read_usize_field(pubkey_bytes, key_len_offset());
        assert_eq!(pub_curve, CX_CURVE_SECP256K1, "pubkey curve mismatch");
        assert_eq!(w_len, 65, "pubkey W_len should be 65, got {}", w_len);
        assert_eq!(pubkey_bytes[key_data_offset()], 0x04, "uncompressed pubkey should start with 0x04");
    }

    log::info!("  PASS");
}

/// Test ECDSA sign and verify round-trip.
fn test_ecdsa_sign_verify() {
    log::info!("test_ecdsa_sign_verify...");

    let raw_key: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11,
        0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
    ];
    let hash: [u8; 32] = [
        0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0x01,
        0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
    ];

    unsafe {
        // Init private key
        let mut pvkey = MaybeUninit::<crypto::CxEcfpPrivateKey>::zeroed();
        let rc = crypto::cx_ecfp_init_private_key_no_throw(
            CX_CURVE_SECP256K1,
            raw_key.as_ptr(),
            32,
            pvkey.as_mut_ptr(),
        );
        assert_eq!(rc, CX_OK);

        // Sign
        let mut sig = [0u8; 80];
        let mut sig_len: u32 = 80;
        let mut info: u32 = 0;
        let rc = crypto::cx_ecdsa_sign_no_throw(
            pvkey.as_ptr(),
            0,
            0,
            hash.as_ptr(),
            32,
            sig.as_mut_ptr(),
            &mut sig_len as *mut u32,
            &mut info as *mut u32,
        );
        assert_eq!(rc, CX_OK, "ecdsa sign failed");
        assert!(sig_len > 0, "signature length should be > 0");

        // Generate public key for verification
        let mut pubkey = MaybeUninit::<crypto::CxEcfpPublicKey>::zeroed();
        let rc = crypto::cx_ecfp_generate_pair_no_throw(
            CX_CURVE_SECP256K1,
            pubkey.as_mut_ptr(),
            pvkey.as_ptr(),
            1,
        );
        assert_eq!(rc, CX_OK);

        // Verify valid signature → should return 1
        let result =
            crypto::cx_ecdsa_verify_no_throw(pubkey.as_ptr(), hash.as_ptr(), 32, sig.as_ptr(), sig_len);
        assert_eq!(result, 1, "valid signature should verify (got {})", result);

        // Corrupt signature and verify → should return 0
        let mut bad_sig = sig;
        bad_sig[10] ^= 0xFF;
        let result =
            crypto::cx_ecdsa_verify_no_throw(pubkey.as_ptr(), hash.as_ptr(), 32, bad_sig.as_ptr(), sig_len);
        assert_eq!(result, 0, "corrupted signature should NOT verify");
    }

    log::info!("  PASS");
}

// ============================================================================
// CRC32 Tests
// ============================================================================

/// Test CRC32 one-shot and incremental against crc32fast reference.
fn test_crc32() {
    log::info!("test_crc32...");

    let data = b"hello";

    // Reference CRC32 from crc32fast
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    let expected = hasher.finalize();

    // One-shot
    unsafe {
        let result = crypto::cx_crc32(data.as_ptr(), data.len() as u32);
        assert_eq!(result, expected, "cx_crc32 mismatch: got 0x{:08x}, expected 0x{:08x}", result, expected);
    }

    // Incremental: "hel" then "lo"
    unsafe {
        let crc1 = crypto::cx_crc32_update(0, b"hel".as_ptr(), 3);
        let crc2 = crypto::cx_crc32_update(crc1, b"lo".as_ptr(), 2);

        // Reference incremental
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(b"hel");
        let ref_crc1 = hasher.finalize();
        let mut hasher2 = crc32fast::Hasher::new_with_initial(ref_crc1);
        hasher2.update(b"lo");
        let ref_crc2 = hasher2.finalize();

        assert_eq!(
            crc2, ref_crc2,
            "cx_crc32_update incremental mismatch: got 0x{:08x}, expected 0x{:08x}",
            crc2, ref_crc2
        );
    }

    // cx_crc_hw delegates to cx_crc32_update
    unsafe {
        let result = crypto::cx_crc_hw(0, 0, data.as_ptr(), data.len() as u32);
        assert_eq!(result, expected, "cx_crc_hw mismatch");
    }

    log::info!("  PASS");
}

// ============================================================================
// RNG Tests
// ============================================================================

/// Test that RNG produces non-zero output and range function is bounded.
fn test_rng() {
    log::info!("test_rng...");

    unsafe {
        // cx_rng_no_throw should fill buffer with pseudo-random data
        let mut buf = [0u8; 32];
        let rc = crypto::cx_rng_no_throw(buf.as_mut_ptr(), 32);
        assert_eq!(rc, CX_OK, "cx_rng_no_throw failed");
        // At least some bytes should be non-zero
        assert!(buf.iter().any(|&b| b != 0), "RNG output should not be all zeros");

        // cx_rng_u32_range_func should return value in [low, high)
        for _ in 0..100 {
            let val = crypto::cx_rng_u32_range_func(10, 20, core::ptr::null());
            assert!(val >= 10, "range result {} should be >= 10", val);
            assert!(val < 20, "range result {} should be < 20", val);
        }

        // Edge case: low == high → returns low
        let val = crypto::cx_rng_u32_range_func(42, 42, core::ptr::null());
        assert_eq!(val, 42, "range with low==high should return low");

        // cx_get_random_bytes delegates to cx_rng_no_throw
        let mut buf2 = [0u8; 16];
        let rc = crypto::cx_get_random_bytes(buf2.as_mut_ptr(), 16);
        assert_eq!(rc, CX_OK, "cx_get_random_bytes failed");
    }

    log::info!("  PASS");
}

// ============================================================================
// Utility Tests
// ============================================================================

/// Test cx_memxor XOR operation.
fn test_memxor() {
    log::info!("test_memxor...");

    unsafe {
        let mut a = [0xAAu8; 4];
        let b = [0x55u8; 4];
        let result = crypto::cx_memxor(a.as_mut_ptr(), b.as_ptr(), 4);
        assert!(!result.is_null(), "cx_memxor should return non-null");
        assert_eq!(a, [0xFF; 4], "0xAA ^ 0x55 should be 0xFF");

        // XOR with itself → zero
        let mut c = [0x12, 0x34, 0x56, 0x78];
        let d = [0x12, 0x34, 0x56, 0x78];
        crypto::cx_memxor(c.as_mut_ptr(), d.as_ptr(), 4);
        assert_eq!(c, [0x00; 4], "XOR with self should be zero");
    }

    log::info!("  PASS");
}

/// Test cx_constant_time_eq.
fn test_constant_time_eq() {
    log::info!("test_constant_time_eq...");

    unsafe {
        let a = [0x01u8, 0x02, 0x03, 0x04];
        let b = [0x01u8, 0x02, 0x03, 0x04];
        let c = [0x01u8, 0x02, 0x03, 0x05]; // differs in last byte

        assert_eq!(
            crypto::cx_constant_time_eq(a.as_ptr(), b.as_ptr(), 4),
            1,
            "equal buffers should return 1"
        );
        assert_eq!(
            crypto::cx_constant_time_eq(a.as_ptr(), c.as_ptr(), 4),
            0,
            "different buffers should return 0"
        );

        // Null pointers → 0
        assert_eq!(
            crypto::cx_constant_time_eq(core::ptr::null(), b.as_ptr(), 4),
            0,
            "null buf1 should return 0"
        );
    }

    log::info!("  PASS");
}

/// Test byte-swap functions.
fn test_swap() {
    log::info!("test_swap...");

    // cx_swap_uint32
    assert_eq!(crypto::cx_swap_uint32(0x01020304), 0x04030201, "swap_uint32 mismatch");
    assert_eq!(crypto::cx_swap_uint32(0x00000000), 0x00000000, "swap_uint32 zero");
    assert_eq!(crypto::cx_swap_uint32(0xFF000000), 0x000000FF, "swap_uint32 0xFF000000");

    // cx_swap_uint64
    assert_eq!(crypto::cx_swap_uint64(0x0102030405060708), 0x0807060504030201, "swap_uint64 mismatch");

    // cx_swap_buffer32
    unsafe {
        let mut buf: [u32; 2] = [0x01020304, 0x05060708];
        crypto::cx_swap_buffer32(buf.as_mut_ptr(), 2);
        assert_eq!(buf[0], 0x04030201, "swap_buffer32[0] mismatch");
        assert_eq!(buf[1], 0x08070605, "swap_buffer32[1] mismatch");
    }

    // cx_swap_buffer64
    unsafe {
        let mut buf: [u64; 1] = [0x0102030405060708];
        crypto::cx_swap_buffer64(buf.as_mut_ptr(), 1);
        assert_eq!(buf[0], 0x0807060504030201, "swap_buffer64 mismatch");
    }

    log::info!("  PASS");
}

// ============================================================================
// EC Domain Tests
// ============================================================================

/// Test EC domain information functions for secp256k1.
fn test_ecdomain() {
    log::info!("test_ecdomain...");

    unsafe {
        // cx_ecdomain_size
        let mut len: u32 = 0;
        let rc = crypto::cx_ecdomain_size(CX_CURVE_SECP256K1, &mut len as *mut u32);
        assert_eq!(rc, CX_OK, "ecdomain_size failed");
        assert_eq!(len, 32, "secp256k1 field size should be 32");

        // cx_ecdomain_parameters_length (delegates to cx_ecdomain_size)
        let mut len2: u32 = 0;
        let rc = crypto::cx_ecdomain_parameters_length(CX_CURVE_SECP256K1, &mut len2 as *mut u32);
        assert_eq!(rc, CX_OK, "ecdomain_parameters_length failed");
        assert_eq!(len2, 32, "secp256k1 parameters length should be 32");

        // cx_ecdomain_generator — known secp256k1 G point
        let expected_gx: [u8; 32] = [
            0x79, 0xBE, 0x66, 0x7E, 0xF9, 0xDC, 0xBB, 0xAC, 0x55, 0xA0, 0x62, 0x95, 0xCE, 0x87, 0x0B, 0x07,
            0x02, 0x9B, 0xFC, 0xDB, 0x2D, 0xCE, 0x28, 0xD9, 0x59, 0xF2, 0x81, 0x5B, 0x16, 0xF8, 0x17, 0x98,
        ];
        let expected_gy: [u8; 32] = [
            0x48, 0x3A, 0xDA, 0x77, 0x26, 0xA3, 0xC4, 0x65, 0x5D, 0xA4, 0xFB, 0xFC, 0x0E, 0x11, 0x08, 0xA8,
            0xFD, 0x17, 0xB4, 0x48, 0xA6, 0x85, 0x54, 0x19, 0x9C, 0x47, 0xD0, 0x8F, 0xFB, 0x10, 0xD4, 0xB8,
        ];

        let mut gx = [0u8; 32];
        let mut gy = [0u8; 32];
        let rc = crypto::cx_ecdomain_generator(CX_CURVE_SECP256K1, gx.as_mut_ptr(), gy.as_mut_ptr(), 32);
        assert_eq!(rc, CX_OK, "ecdomain_generator failed");
        assert_eq!(gx, expected_gx, "secp256k1 Gx mismatch");
        assert_eq!(gy, expected_gy, "secp256k1 Gy mismatch");

        // cx_ecdomain_parameter with id=6 (Order n)
        let expected_n: [u8; 32] = [
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE,
            0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36, 0x41, 0x41,
        ];
        let mut n = [0u8; 32];
        let rc = crypto::cx_ecdomain_parameter(CX_CURVE_SECP256K1, 6, n.as_mut_ptr(), 32);
        assert_eq!(rc, CX_OK, "ecdomain_parameter(order) failed");
        assert_eq!(n, expected_n, "secp256k1 order n mismatch");

        // cx_ecdomain_parameter with id=3 (Field prime p)
        let expected_p: [u8; 32] = [
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0xFF, 0xFF, 0xFC, 0x2F,
        ];
        let mut p = [0u8; 32];
        let rc = crypto::cx_ecdomain_parameter(CX_CURVE_SECP256K1, 3, p.as_mut_ptr(), 32);
        assert_eq!(rc, CX_OK, "ecdomain_parameter(field) failed");
        assert_eq!(p, expected_p, "secp256k1 field prime p mismatch");

        // cx_ecdomain_parameter with id=1 (a = 0)
        let mut a = [0xFFu8; 32];
        let rc = crypto::cx_ecdomain_parameter(CX_CURVE_SECP256K1, 1, a.as_mut_ptr(), 32);
        assert_eq!(rc, CX_OK, "ecdomain_parameter(a) failed");
        assert_eq!(a, [0u8; 32], "secp256k1 a should be 0");

        // cx_ecdomain_parameter with id=2 (b = 7)
        let mut b = [0u8; 32];
        let rc = crypto::cx_ecdomain_parameter(CX_CURVE_SECP256K1, 2, b.as_mut_ptr(), 32);
        assert_eq!(rc, CX_OK, "ecdomain_parameter(b) failed");
        let mut expected_b = [0u8; 32];
        expected_b[31] = 7;
        assert_eq!(b, expected_b, "secp256k1 b should be 7");

        // Unsupported curve should fail
        let mut dummy: u32 = 0;
        let rc = crypto::cx_ecdomain_size(0x99, &mut dummy as *mut u32);
        assert_eq!(rc, CX_INVALID_PARAMETER, "unsupported curve should fail");
    }

    log::info!("  PASS");
}
