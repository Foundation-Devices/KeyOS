// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Key derivation and EC operations for Flux syscall emulation.
//!
//! Provides BIP32 key derivation using KeyOS's ngwallet infrastructure
//! and ECDSA operations using the secp256k1 crate.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        LazyLock, RwLock,
    },
};

use ngwallet::bdk_wallet::bitcoin::bip32::{DerivationPath, Xpriv};
use secp256k1::{PublicKey, Secp256k1, SecretKey};

/// SDK ECDSA sign info flags.
#[allow(unused, non_upper_case_globals)]
pub mod ecdsa_info {
    /// The y-coordinate of the signature's R point is odd.
    pub const CX_ECCINFO_PARITY_ODD: u32 = 0x01;
    /// The x-coordinate of the signature's R point is greater than the curve order n.
    pub const CX_ECCINFO_xGTn: u32 = 0x02;
}

/// Flux SDK curve identifiers.
#[allow(unused)]
pub mod curves {
    pub const CX_CURVE_NONE: u8 = 0x00;
    pub const CX_CURVE_SECP256K1: u8 = 0x21;
    pub const CX_CURVE_SECP256R1: u8 = 0x22;
    pub const CX_CURVE_SECP384R1: u8 = 0x23;
    pub const CX_CURVE_BLS12_381_G1: u8 = 0x39;
    pub const CX_CURVE_ED25519: u8 = 0x71;
    pub const CX_CURVE_ED448: u8 = 0x27;
    pub const CX_CURVE_CURVE25519: u8 = 0x06;
}

/// Key context ID counter for unique context allocation.
static NEXT_KEY_ID: AtomicU32 = AtomicU32::new(1);

/// Global EC key context storage.
static EC_CONTEXTS: LazyLock<RwLock<HashMap<u32, EcContext>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Derived key storage (from BIP32 derivation).
static DERIVED_KEYS: LazyLock<RwLock<HashMap<u32, DerivedKey>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// EC key context for ECDSA operations.
pub struct EcContext {
    /// The curve identifier (e.g., CX_CURVE_SECP256K1).
    pub curve: u8,
    /// The private key (32 bytes for secp256k1).
    pub private_key: [u8; 32],
    /// The public key (33 bytes compressed or 65 bytes uncompressed).
    pub public_key: Option<Vec<u8>>,
}

/// A derived key from BIP32 derivation.
pub struct DerivedKey {
    /// The chain code (32 bytes).
    pub chain_code: [u8; 32],
    /// The private key (32 bytes).
    pub private_key: [u8; 32],
}

/// Errors that can occur during key operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// The specified context ID was not found.
    ContextNotFound,
    /// The key storage lock was poisoned.
    LockPoisoned,
    /// Unsupported curve type.
    UnsupportedCurve,
    /// Invalid key data.
    InvalidKey,
    /// BIP32 derivation failed.
    DerivationFailed,
    /// Seed not available.
    SeedNotAvailable,
    /// Invalid derivation path.
    InvalidPath,
}

/// Initialize a private key context from raw key bytes.
///
/// # Arguments
/// * `curve` - The curve identifier (e.g., CX_CURVE_SECP256K1 or CX_CURVE_ED25519)
/// * `raw_key` - The raw private key bytes (32 bytes)
///
/// # Returns
/// A unique context ID or 0 on error.
pub fn init_private_key(curve: u8, raw_key: &[u8]) -> u32 {
    if raw_key.len() != 32 {
        log::warn!("Invalid private key length: {} (expected 32)", raw_key.len());
        return 0;
    }

    match curve {
        curves::CX_CURVE_SECP256K1 => {
            // Validate the key is a valid secp256k1 scalar
            if SecretKey::from_slice(raw_key).is_err() {
                log::warn!("Invalid secp256k1 private key");
                return 0;
            }
        }
        curves::CX_CURVE_ED25519 => {
            // Any 32-byte value is a valid Ed25519 private key seed
        }
        _ => {
            log::warn!("Unsupported curve: 0x{:02x}", curve);
            return 0;
        }
    }

    let id = NEXT_KEY_ID.fetch_add(1, Ordering::Relaxed);
    let mut private_key = [0u8; 32];
    private_key.copy_from_slice(raw_key);

    if let Ok(mut contexts) = EC_CONTEXTS.write() {
        contexts.insert(id, EcContext { curve, private_key, public_key: None });
        log::debug!("Created EC private key context with id={} curve=0x{:02x}", id, curve);
    } else {
        log::error!("Failed to acquire EC contexts write lock");
        return 0;
    }

    id
}

/// Generate a public key from an existing private key context.
///
/// # Arguments
/// * `ctx_id` - The private key context ID
/// * `compressed` - Whether to generate a compressed (33-byte) public key
///
/// # Returns
/// `Ok(())` on success, or an error if the context was not found.
pub fn generate_pair(ctx_id: u32, compressed: bool) -> Result<(), KeyError> {
    let mut contexts = EC_CONTEXTS.write().map_err(|_| KeyError::LockPoisoned)?;
    let ctx = contexts.get_mut(&ctx_id).ok_or(KeyError::ContextNotFound)?;

    match ctx.curve {
        curves::CX_CURVE_SECP256K1 => {
            let secp = Secp256k1::new();
            let secret_key = SecretKey::from_slice(&ctx.private_key).map_err(|_| KeyError::InvalidKey)?;
            let public_key = PublicKey::from_secret_key(&secp, &secret_key);

            let pubkey_bytes = if compressed {
                public_key.serialize().to_vec()
            } else {
                public_key.serialize_uncompressed().to_vec()
            };
            ctx.public_key = Some(pubkey_bytes);
        }
        curves::CX_CURVE_ED25519 => {
            let pubkey = ed25519_generate_pubkey(&ctx.private_key);
            ctx.public_key = Some(pubkey.to_vec());
        }
        _ => return Err(KeyError::UnsupportedCurve),
    }

    log::debug!(
        "Generated {} public key for context {}",
        if compressed { "compressed" } else { "uncompressed" },
        ctx_id
    );

    Ok(())
}

/// Get the public key from an EC context.
///
/// # Arguments
/// * `ctx_id` - The EC context ID
///
/// # Returns
/// The public key bytes, or an error if not generated yet.
pub fn get_public_key(ctx_id: u32) -> Result<Vec<u8>, KeyError> {
    let contexts = EC_CONTEXTS.read().map_err(|_| KeyError::LockPoisoned)?;
    let ctx = contexts.get(&ctx_id).ok_or(KeyError::ContextNotFound)?;

    ctx.public_key.clone().ok_or(KeyError::InvalidKey)
}

/// Get the private key from an EC context.
///
/// # Arguments
/// * `ctx_id` - The EC context ID
///
/// # Returns
/// The private key bytes (32 bytes).
pub fn get_private_key(ctx_id: u32) -> Result<[u8; 32], KeyError> {
    let contexts = EC_CONTEXTS.read().map_err(|_| KeyError::LockPoisoned)?;
    let ctx = contexts.get(&ctx_id).ok_or(KeyError::ContextNotFound)?;

    Ok(ctx.private_key)
}

/// Destroy an EC key context.
///
/// # Arguments
/// * `ctx_id` - The context ID to destroy
pub fn destroy_ec_context(ctx_id: u32) -> Result<(), KeyError> {
    let mut contexts = EC_CONTEXTS.write().map_err(|_| KeyError::LockPoisoned)?;
    if contexts.remove(&ctx_id).is_some() {
        log::debug!("Destroyed EC context {}", ctx_id);
        Ok(())
    } else {
        Err(KeyError::ContextNotFound)
    }
}

/// Initialize a public key context from raw key bytes.
///
/// # Arguments
/// * `curve` - The curve identifier
/// * `raw_key` - The raw public key bytes (33 compressed or 65 uncompressed)
///
/// # Returns
/// A unique context ID or 0 on error.
pub fn init_public_key(curve: u8, raw_key: &[u8]) -> u32 {
    if curve != curves::CX_CURVE_SECP256K1 {
        log::warn!("Unsupported curve: 0x{:02x}", curve);
        return 0;
    }

    // Validate public key format
    if raw_key.len() != 33 && raw_key.len() != 65 {
        log::warn!("Invalid public key length: {} (expected 33 or 65)", raw_key.len());
        return 0;
    }

    // Validate the key is a valid secp256k1 point
    if PublicKey::from_slice(raw_key).is_err() {
        log::warn!("Invalid secp256k1 public key");
        return 0;
    }

    let id = NEXT_KEY_ID.fetch_add(1, Ordering::Relaxed);

    if let Ok(mut contexts) = EC_CONTEXTS.write() {
        contexts.insert(
            id,
            EcContext {
                curve,
                private_key: [0u8; 32], // No private key for public-only context
                public_key: Some(raw_key.to_vec()),
            },
        );
        log::debug!("Created EC public key context with id={}", id);
    } else {
        log::error!("Failed to acquire EC contexts write lock");
        return 0;
    }

    id
}

/// Verify an ECDSA signature.
///
/// # Arguments
/// * `pubkey_ctx_id` - The public key context ID
/// * `hash` - The message hash (32 bytes for secp256k1)
/// * `signature` - The DER-encoded signature
///
/// # Returns
/// `true` if the signature is valid, `false` otherwise.
pub fn ecdsa_verify(pubkey_ctx_id: u32, hash: &[u8], signature: &[u8]) -> bool {
    let contexts = match EC_CONTEXTS.read() {
        Ok(c) => c,
        Err(_) => return false,
    };

    let ctx = match contexts.get(&pubkey_ctx_id) {
        Some(c) => c,
        None => return false,
    };

    let pubkey_bytes = match &ctx.public_key {
        Some(p) => p,
        None => return false,
    };

    let secp = Secp256k1::new();

    let public_key = match PublicKey::from_slice(pubkey_bytes) {
        Ok(k) => k,
        Err(_) => return false,
    };

    let message = match secp256k1::Message::from_digest_slice(hash) {
        Ok(m) => m,
        Err(_) => return false,
    };

    let sig = match secp256k1::ecdsa::Signature::from_der(signature) {
        Ok(s) => s,
        Err(_) => return false,
    };

    secp.verify_ecdsa(&message, &sig, &public_key).is_ok()
}

/// Decompress a SEC1 compressed secp256k1 point (02/03 prefix + X) to the
/// uncompressed 04 || X || Y form.
pub fn secp256k1_decompress(compressed: &[u8]) -> Option<[u8; 65]> {
    let key = PublicKey::from_slice(compressed).ok()?;
    Some(key.serialize_uncompressed())
}

/// Verify a DER-encoded ECDSA signature over a 32-byte digest with a raw
/// secp256k1 public key (compressed or uncompressed form).
///
/// High-S signatures are normalized before verification: the SDK's verifier
/// accepts them, while rust-secp256k1 alone would reject.
pub fn secp256k1_verify_der(pubkey: &[u8], digest32: &[u8], der_sig: &[u8]) -> bool {
    let secp = Secp256k1::verification_only();
    let Ok(key) = PublicKey::from_slice(pubkey) else {
        return false;
    };
    let Ok(message) = secp256k1::Message::from_digest_slice(digest32) else {
        return false;
    };
    let Ok(mut sig) = secp256k1::ecdsa::Signature::from_der(der_sig) else {
        return false;
    };
    sig.normalize_s();
    secp.verify_ecdsa(&message, &sig, &key).is_ok()
}

/// Sign a message hash with ECDSA.
///
/// # Arguments
/// * `privkey_ctx_id` - The private key context ID
/// * `hash` - The message hash (32 bytes)
///
/// # Returns
/// The DER-encoded signature, or an error.
pub fn ecdsa_sign(privkey_ctx_id: u32, hash: &[u8]) -> Result<Vec<u8>, KeyError> {
    let contexts = EC_CONTEXTS.read().map_err(|_| KeyError::LockPoisoned)?;
    let ctx = contexts.get(&privkey_ctx_id).ok_or(KeyError::ContextNotFound)?;

    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(&ctx.private_key).map_err(|_| KeyError::InvalidKey)?;
    let message = secp256k1::Message::from_digest_slice(hash).map_err(|_| KeyError::InvalidKey)?;

    let signature = secp.sign_ecdsa(&message, &secret_key);
    Ok(signature.serialize_der().to_vec())
}

/// Sign a message hash with ECDSA, returning the DER signature and recovery info.
///
/// The `info` output contains SDK-compatible flags:
/// - `CX_ECCINFO_PARITY_ODD` (0x01): the y-coordinate of R is odd
/// - `CX_ECCINFO_xGTn` (0x02): the x-coordinate of R is greater than curve order n
///
/// # Arguments
/// * `privkey_ctx_id` - The private key context ID
/// * `hash` - The message hash (32 bytes)
///
/// # Returns
/// A tuple of (DER-encoded signature, info flags), or an error.
pub fn ecdsa_sign_recoverable(privkey_ctx_id: u32, hash: &[u8]) -> Result<(Vec<u8>, u32), KeyError> {
    let contexts = EC_CONTEXTS.read().map_err(|_| KeyError::LockPoisoned)?;
    let ctx = contexts.get(&privkey_ctx_id).ok_or(KeyError::ContextNotFound)?;

    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(&ctx.private_key).map_err(|_| KeyError::InvalidKey)?;
    let message = secp256k1::Message::from_digest_slice(hash).map_err(|_| KeyError::InvalidKey)?;

    let recoverable_sig = secp.sign_ecdsa_recoverable(&message, &secret_key);
    let (recovery_id, compact) = recoverable_sig.serialize_compact();

    // Convert to standard signature for DER encoding
    let standard_sig = recoverable_sig.to_standard();
    let der = standard_sig.serialize_der().to_vec();

    // Build info flags from recovery ID
    // Recovery ID encodes: bit 0 = parity of R.y, bit 1 = R.x > n
    let mut info: u32 = 0;
    let recid = recovery_id.to_i32();
    if recid & 1 != 0 {
        info |= ecdsa_info::CX_ECCINFO_PARITY_ODD;
    }
    if recid & 2 != 0 {
        info |= ecdsa_info::CX_ECCINFO_xGTn;
    }

    // Also check parity directly from the compact R value
    // The compact signature format is [R (32 bytes) || S (32 bytes)]
    // We can verify our parity flag by checking the actual R.y parity
    let _ = compact; // R is in compact[0..32], used above via recovery_id

    log::debug!("ECDSA recoverable sign: recovery_id={}, info=0x{:02x}", recid, info);
    Ok((der, info))
}

/// Derive a BIP32 key from a seed.
///
/// # Arguments
/// * `seed` - The master seed (64 bytes typically, or 32-byte app seed)
/// * `path` - The BIP32 derivation path components (each u32 in path array)
/// * `curve` - The target curve (must be CX_CURVE_SECP256K1)
///
/// # Returns
/// A tuple of (private_key, chain_code) or an error.
pub fn derive_bip32(seed: &[u8], path: &[u32], curve: u8) -> Result<([u8; 32], [u8; 32]), KeyError> {
    if curve != curves::CX_CURVE_SECP256K1 {
        log::warn!("BIP32 derivation only supports secp256k1, got curve: 0x{:02x}", curve);
        return Err(KeyError::UnsupportedCurve);
    }

    // For app_seed which is 32 bytes, we need to extend it to 64 bytes
    // by using SHA-512 or padding. ngwallet expects a 64-byte seed.
    let extended_seed: Vec<u8> = if seed.len() == 32 {
        // Use the 32-byte seed directly to create master key
        // SHA-512(seed) produces 64 bytes: first 32 = key, last 32 = chain code
        use sha2::{Digest, Sha512};
        let mut hasher = Sha512::new();
        hasher.update(b"Bitcoin seed");
        hasher.update(seed);
        hasher.finalize().to_vec()
    } else if seed.len() == 64 {
        seed.to_vec()
    } else {
        log::warn!("Invalid seed length: {} (expected 32 or 64)", seed.len());
        return Err(KeyError::InvalidKey);
    };

    // Create master key from seed
    let xpriv =
        Xpriv::new_master(ngwallet::bdk_wallet::bitcoin::Network::Bitcoin, &extended_seed).map_err(|e| {
            log::warn!("Failed to create master key: {:?}", e);
            KeyError::DerivationFailed
        })?;

    // Build derivation path
    let path_str = format!(
        "m/{}",
        path.iter()
            .map(|&p| {
                if p & 0x80000000 != 0 {
                    format!("{}'", p & 0x7FFFFFFF)
                } else {
                    format!("{}", p)
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    );

    let derivation_path: DerivationPath = path_str.parse().map_err(|e| {
        log::warn!("Failed to parse derivation path '{}': {:?}", path_str, e);
        KeyError::InvalidPath
    })?;

    log::debug!("Deriving key at path: {}", derivation_path);

    let secp = Secp256k1::new();
    let derived = xpriv.derive_priv(&secp, &derivation_path).map_err(|e| {
        log::warn!("BIP32 derivation failed: {:?}", e);
        KeyError::DerivationFailed
    })?;

    let private_key: [u8; 32] = derived.private_key.secret_bytes();
    let chain_code: [u8; 32] = derived.chain_code.to_bytes();

    log::debug!("Successfully derived key at path: {}", derivation_path);
    Ok((private_key, chain_code))
}

/// Store a derived key and return a context ID.
///
/// # Arguments
/// * `private_key` - The 32-byte private key
/// * `chain_code` - The 32-byte chain code
///
/// # Returns
/// A unique context ID.
pub fn store_derived_key(private_key: [u8; 32], chain_code: [u8; 32]) -> u32 {
    let id = NEXT_KEY_ID.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut keys) = DERIVED_KEYS.write() {
        keys.insert(id, DerivedKey { chain_code, private_key });
        log::debug!("Stored derived key with id={}", id);
    }
    id
}

/// Get a derived key by context ID.
pub fn get_derived_key(ctx_id: u32) -> Result<(Vec<u8>, Vec<u8>), KeyError> {
    let keys = DERIVED_KEYS.read().map_err(|_| KeyError::LockPoisoned)?;
    let key = keys.get(&ctx_id).ok_or(KeyError::ContextNotFound)?;
    Ok((key.private_key.to_vec(), key.chain_code.to_vec()))
}

/// Derive a key using SLIP-10 for Ed25519.
///
/// SLIP-10 specifies hardened-only derivation for Ed25519:
/// - Master: HMAC-SHA512(key="ed25519 seed", data=seed)
/// - Child:  HMAC-SHA512(key=chain_code, data=0x00 || privkey || index_BE)
///
/// # Arguments
/// * `seed` - The master seed bytes (any length, typically 16-64 bytes)
/// * `path` - Derivation path components (each must have bit 31 set = hardened)
///
/// # Returns
/// (private_key, chain_code) or an error.
pub fn derive_slip10_ed25519(seed: &[u8], path: &[u32]) -> Result<([u8; 32], [u8; 32]), KeyError> {
    use hmac::{Hmac, Mac};
    use sha2::Sha512;

    // Master key: HMAC-SHA512(key="ed25519 seed", data=seed)
    let mut mac = Hmac::<Sha512>::new_from_slice(b"ed25519 seed").map_err(|_| KeyError::DerivationFailed)?;
    mac.update(seed);
    let result = mac.finalize().into_bytes();

    let mut key = [0u8; 32];
    let mut chain_code = [0u8; 32];
    key.copy_from_slice(&result[..32]);
    chain_code.copy_from_slice(&result[32..]);

    log::debug!("SLIP-10 Ed25519 master key derived for path length {}", path.len());

    // Derive each child level
    for &index in path {
        if index & 0x80000000 == 0 {
            log::warn!("SLIP-10 Ed25519: non-hardened index 0x{:08x} not supported", index);
            return Err(KeyError::InvalidPath);
        }

        let mut mac = Hmac::<Sha512>::new_from_slice(&chain_code).map_err(|_| KeyError::DerivationFailed)?;
        mac.update(&[0x00]);
        mac.update(&key);
        mac.update(&index.to_be_bytes());
        let result = mac.finalize().into_bytes();

        key.copy_from_slice(&result[..32]);
        chain_code.copy_from_slice(&result[32..]);
    }

    Ok((key, chain_code))
}

/// Generate an Ed25519 public key in SDK format from a private key seed.
///
/// The SDK stores Ed25519 public keys as 65 bytes:
///   `[0x04 || X_BE(32) || Y_BE(32)]`
///
/// The Solana app converts this to the standard 32-byte compressed Ed25519 form
/// by reversing Y to little-endian and setting the X parity bit.
fn ed25519_generate_pubkey(privkey: &[u8; 32]) -> [u8; 65] {
    use ed25519_dalek::SigningKey;

    let signing_key = SigningKey::from_bytes(privkey);
    let verifying_key = signing_key.verifying_key();
    let compressed = verifying_key.to_bytes(); // 32-byte standard compressed form

    // Standard compressed Ed25519: Y_LE(32) with X sign bit in top bit of last byte
    let x_sign = compressed[31] >> 7;
    let mut y_le = compressed;
    y_le[31] &= 0x7F; // Clear sign bit to get pure Y

    // Convert Y to big-endian
    let mut y_be = y_le;
    y_be.reverse();

    // Build SDK format: [0x04 || X_BE(32) || Y_BE(32)]
    // The Solana app only reads W[32] for X parity and W[33..65] for Y_BE,
    // so we set X_BE to zeros except for the parity bit in the last byte.
    let mut result = [0u8; 65];
    result[0] = 0x04;
    result[32] = x_sign; // X_BE[31] — only LSB (parity) is used
    result[33..65].copy_from_slice(&y_be);

    log::debug!("Ed25519 pubkey generated: compressed={:02x?}", &compressed[..4]);

    result
}

/// Sign a message using Ed25519 (EdDSA).
///
/// # Arguments
/// * `privkey` - The 32-byte Ed25519 private key seed
/// * `message` - The raw message bytes (Ed25519 hashes internally with SHA-512)
///
/// # Returns
/// The 64-byte Ed25519 signature.
pub fn eddsa_sign(privkey: &[u8; 32], message: &[u8]) -> Result<[u8; 64], KeyError> {
    use ed25519_dalek::{Signer, SigningKey};

    let signing_key = SigningKey::from_bytes(privkey);
    let signature = signing_key.sign(message);

    log::debug!("Ed25519 sign: message_len={}, sig={:02x?}", message.len(), &signature.to_bytes()[..4]);

    Ok(signature.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_private_key() {
        // Valid secp256k1 private key (32 bytes, valid scalar)
        let valid_key = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ];

        let id = init_private_key(curves::CX_CURVE_SECP256K1, &valid_key);
        assert!(id > 0);
    }

    #[test]
    fn test_generate_pair() {
        let valid_key = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ];

        let id = init_private_key(curves::CX_CURVE_SECP256K1, &valid_key);
        assert!(id > 0);

        // Generate compressed public key
        assert!(generate_pair(id, true).is_ok());

        let pubkey = get_public_key(id).unwrap();
        assert_eq!(pubkey.len(), 33); // Compressed public key is 33 bytes
    }

    #[test]
    fn test_derive_bip32() {
        // Test seed (64 bytes)
        let seed = [0u8; 64];
        // BIP44 Ethereum path: m/44'/60'/0'/0/0
        let path = [
            0x8000002C, // 44'
            0x8000003C, // 60'
            0x80000000, // 0'
            0x00000000, // 0
            0x00000000, // 0
        ];

        let result = derive_bip32(&seed, &path, curves::CX_CURVE_SECP256K1);
        assert!(result.is_ok());

        let (private_key, chain_code) = result.unwrap();
        assert_eq!(private_key.len(), 32);
        assert_eq!(chain_code.len(), 32);
    }

    #[test]
    fn test_ecdsa_sign_verify() {
        let valid_key = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ];

        let id = init_private_key(curves::CX_CURVE_SECP256K1, &valid_key);
        generate_pair(id, true).unwrap();

        let hash = [0u8; 32];
        let signature = ecdsa_sign(id, &hash).unwrap();

        // Verify with the same context (has public key)
        assert!(ecdsa_verify(id, &hash, &signature));
    }

    #[test]
    fn test_slip10_ed25519_vector1_master() {
        // SLIP-10 test vector 1: seed = 000102030405060708090a0b0c0d0e0f
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();

        let (key, chain_code) = derive_slip10_ed25519(&seed, &[]).unwrap();

        let expected_key =
            hex::decode("2b4be7f19ee27bbf30c667b642d5f4aa69fd169872f8fc3059c08ebae2eb19e7").unwrap();
        let expected_chain =
            hex::decode("90046a93de5380a72b5e45010748567d5ea02bbf6522f979e05c0d8d8ca9fffb").unwrap();

        assert_eq!(key[..], expected_key[..]);
        assert_eq!(chain_code[..], expected_chain[..]);
    }

    #[test]
    fn test_slip10_ed25519_vector1_child() {
        // SLIP-10 test vector 1: chain m/0H → m/0H/1H
        // Verify m/0H/1H against the spec (chain code + deeper derivation confirms m/0H)
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();

        let (key, chain_code) = derive_slip10_ed25519(&seed, &[0x80000000, 0x80000001]).unwrap();

        let expected_key =
            hex::decode("b1d0bad404bf35da785a64ca1ac54b2617211d2777696fbffaf208f746ae84f2").unwrap();
        let expected_chain =
            hex::decode("a320425f77d1b5c2505a6b1b27382b37368ee640e3557c315416801243552f14").unwrap();

        assert_eq!(key[..], expected_key[..]);
        assert_eq!(chain_code[..], expected_chain[..]);
    }

    #[test]
    fn test_slip10_ed25519_pubkey() {
        // SLIP-10 test vector 1: master key → Ed25519 public key
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let (key, _chain_code) = derive_slip10_ed25519(&seed, &[]).unwrap();

        // Expected compressed Ed25519 pubkey (from SLIP-10 spec, strip leading 00)
        let expected_pubkey =
            hex::decode("a4b2856bfec510abab89753fac1ac0e1112364e7d250545963f135f2a33188ed").unwrap();

        // Generate public key using ed25519-dalek
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&key);
        let verifying_key = signing_key.verifying_key();
        let compressed = verifying_key.to_bytes();

        assert_eq!(compressed[..], expected_pubkey[..]);
    }

    #[test]
    fn test_ed25519_init_and_generate() {
        let key = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ];

        let id = init_private_key(curves::CX_CURVE_ED25519, &key);
        assert!(id > 0);

        assert!(generate_pair(id, false).is_ok());

        let pubkey = get_public_key(id).unwrap();
        assert_eq!(pubkey.len(), 65);
        assert_eq!(pubkey[0], 0x04); // Uncompressed marker
    }

    #[test]
    fn test_eddsa_sign_verify() {
        // SLIP-10 test vector 1 master key
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let (key, _) = derive_slip10_ed25519(&seed, &[]).unwrap();

        let message = b"test message";
        let sig = eddsa_sign(&key, message).unwrap();
        assert_eq!(sig.len(), 64);

        // Verify with ed25519-dalek
        use ed25519_dalek::{Signature, Verifier};
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&key);
        let verifying_key = signing_key.verifying_key();
        let signature = Signature::from_bytes(&sig);
        assert!(verifying_key.verify(message, &signature).is_ok());
    }

    #[test]
    fn test_slip10_rejects_non_hardened() {
        let seed = [0u8; 16];
        let result = derive_slip10_ed25519(&seed, &[0x00000001]); // non-hardened
        assert_eq!(result, Err(KeyError::InvalidPath));
    }

    #[test]
    fn test_ed25519_sdk_format_roundtrip() {
        // Verify that the SDK format pubkey can be converted back to the
        // standard compressed form (as the Solana C app does).
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let (key, _) = derive_slip10_ed25519(&seed, &[]).unwrap();

        let sdk_pubkey = ed25519_generate_pubkey(&key);

        // Simulate Solana app's conversion: reverse W[33..65] → Y_LE, set X parity
        let mut raw_pubkey = [0u8; 32];
        for i in 0..32 {
            raw_pubkey[i] = sdk_pubkey[64 - i];
        }
        if sdk_pubkey[32] & 1 != 0 {
            raw_pubkey[31] |= 0x80;
        }

        // Compare with direct ed25519-dalek compressed key
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&key);
        let expected = signing_key.verifying_key().to_bytes();

        assert_eq!(raw_pubkey, expected);
    }
}
