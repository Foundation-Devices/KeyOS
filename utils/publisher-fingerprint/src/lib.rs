// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::fmt;

use sha2::{Digest, Sha256};

pub const COMPRESSED_PUBLIC_KEY_LEN: usize = 33;
pub const FULL_FINGERPRINT_HEX_LEN: usize = 64;

/// Canonical identity displayed for an allowed publisher.
///
/// The full form is the lowercase hexadecimal SHA-256 digest of the compressed
/// 33-byte secp256k1 public key. The short form is the first and last four
/// digest bytes separated by an ellipsis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherFingerprint {
    pub full: String,
    pub short: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidCompressedPublicKey;

impl fmt::Display for InvalidCompressedPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Publisher public key must be a compressed 33-byte secp256k1 public key")
    }
}

impl core::error::Error for InvalidCompressedPublicKey {}

impl PublisherFingerprint {
    /// Derive the one canonical publisher fingerprint used by firmware and
    /// host tooling.
    pub fn from_compressed_public_key(public_key: &[u8]) -> Result<Self, InvalidCompressedPublicKey> {
        if public_key.len() != COMPRESSED_PUBLIC_KEY_LEN || !matches!(public_key[0], 0x02 | 0x03) {
            return Err(InvalidCompressedPublicKey);
        }

        let digest = Sha256::digest(public_key);
        let full = lowercase_hex(&digest);
        let short = format!("{}…{}", &full[..8], &full[FULL_FINGERPRINT_HEX_LEN - 8..]);

        Ok(Self { full, short })
    }
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_known_vector() {
        let public_key =
            hex_literal::hex!("0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798");

        let fingerprint = PublisherFingerprint::from_compressed_public_key(&public_key).unwrap();

        assert_eq!(fingerprint.full, "0f715baf5d4c2ed329785cef29e562f73488c8a2bb9dbc5700b361d54b9b0554");
        assert_eq!(fingerprint.short, "0f715baf…4b9b0554");
    }

    #[test]
    fn rejects_uncompressed_or_wrong_length_keys() {
        assert_eq!(
            PublisherFingerprint::from_compressed_public_key(&[0x04; COMPRESSED_PUBLIC_KEY_LEN]),
            Err(InvalidCompressedPublicKey)
        );
        assert_eq!(
            PublisherFingerprint::from_compressed_public_key(&[0x02; COMPRESSED_PUBLIC_KEY_LEN - 1]),
            Err(InvalidCompressedPublicKey)
        );
    }
}
