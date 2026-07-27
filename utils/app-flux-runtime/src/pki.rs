// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! PKI syscall emulation: `os_pki_load_certificate`, `os_pki_get_info`,
//! `os_pki_verify`.
//!
//! Ports the Speculos implementation of the OS PKI: parse the TLV certificate,
//! verify its ECDSA signature against the vendor root CA (or against the
//! previously loaded certificate for chained certificates), then keep the
//! certified public key in RAM so `os_pki_verify` can authenticate descriptors
//! (Transaction Check verdicts, trusted names, ...) the host sends alongside
//! signing requests.

use std::sync::{LazyLock, RwLock};

use gui_app_emu_flux::keys::{self, curves::CX_CURVE_SECP256K1};
use sha2::{Digest, Sha256};
use sha3::{Keccak256, Sha3_256};

// Certificate TLV tags (SDK os_pki.h).
const TAG_STRUCTURE_TYPE: u8 = 0x01;
const TAG_VERSION: u8 = 0x02;
const TAG_VALIDITY: u8 = 0x10;
const TAG_VALIDITY_INDEX: u8 = 0x11;
const TAG_CHALLENGE: u8 = 0x12;
const TAG_SIGNER_KEY_ID: u8 = 0x13;
const TAG_SIGN_ALGO_ID: u8 = 0x14;
const TAG_SIGNATURE: u8 = 0x15;
const TAG_TIME_VALIDITY: u8 = 0x16;
const TAG_TRUSTED_NAME: u8 = 0x20;
const TAG_PUBLIC_KEY_ID: u8 = 0x30;
const TAG_PUBLIC_KEY_USAGE: u8 = 0x31;
const TAG_PUBLIC_KEY_CURVE_ID: u8 = 0x32;
const TAG_COMPRESSED_PUBLIC_KEY: u8 = 0x33;
const TAG_PK_SIGN_ALGO_ID: u8 = 0x34;
const TAG_TARGET_DEVICE: u8 = 0x35;
const TAG_DEPTH: u8 = 0x36;

// Field bounds (SDK os_pki.h).
const STRUCTURE_TYPE_CERTIFICATE: u8 = 0x01;
const CERTIFICATE_VERSION_UNKNOWN: u8 = 0x03;
const SIGN_ALGO_ID_UNKNOWN: u8 = 0x11;
// The pinned SDKs (v26.1.6/v26.4.0) end the target-device enum at APEXP, so
// their UNKNOWN sentinel is 0x07; newer trees add APEX_M and shift it to 0x08.
// Keep in step with the pinned SDK on bumps.
const TARGET_DEVICE_UNKNOWN: u8 = 0x07;
const KEY_ID_LEDGER_ROOT_V3: u16 = 0x0002;
pub const TRUSTED_NAME_MAXLEN: usize = 32;

// Signature algorithm identifiers (SDK os_pki.h).
const SIGN_ALGO_ECDSA_SHA256: u8 = 0x01;
const SIGN_ALGO_ECDSA_SHA3_256: u8 = 0x02;
const SIGN_ALGO_ECDSA_KECCAK_256: u8 = 0x03;

// SWO error codes, matching Speculos/the OS so hosts see familiar statuses.
const SWO_OK: u32 = 0x0000;
const ERR_CRYPTO: u32 = 0x3302;
const ERR_TAG_MALFORMED: u32 = 0x422D;
const ERR_KEY_USAGE_MISMATCH: u32 = 0x422E;
const ERR_STRUCTURE_TYPE: u32 = 0x422F;
const ERR_VERSION: u32 = 0x4230;
const ERR_VALIDITY: u32 = 0x4231;
const ERR_VALIDITY_INDEX: u32 = 0x4232;
const ERR_SIGNER_KEY_ID: u32 = 0x4233;
const ERR_SIGN_ALGO: u32 = 0x4234;
const ERR_PUBLIC_KEY_ID: u32 = 0x4235;
const ERR_KEY_USAGE_FIELD: u32 = 0x4236;
const ERR_CURVE_ID: u32 = 0x4237;
const ERR_PK_SIGN_ALGO: u32 = 0x4238;
const ERR_TARGET_DEVICE: u32 = 0x4239;
const ERR_TRUSTED_NAME: u32 = 0x423A;
const ERR_TIME_VALIDITY: u32 = 0x423B;
const ERR_DEPTH: u32 = 0x423C;
const ERR_SIGNATURE: u32 = 0x5720;
const ERR_NO_CERTIFICATE: u32 = 0x590E;

/// Certificates arrive in a single APDU; this bounds the raw-pointer read
/// against a corrupted length.
const MAX_CERTIFICATE_LEN: usize = 1024;

/// Vendor root CA v3 public key (secp256k1, uncompressed), as baked in the OS.
const ROOT_CA_V3_PROD: [u8; 65] = [
    0x04, 0xe6, 0xb3, 0x51, 0x48, 0xbf, 0x55, 0x6c, 0xa9, 0x0b, 0xac, 0x3a, 0x4e, 0x85, 0xc0, 0x08, 0x1e,
    0x55, 0x81, 0x24, 0xce, 0xeb, 0x3c, 0x7d, 0x72, 0xf9, 0x14, 0x10, 0x0f, 0x8b, 0x2a, 0xd6, 0x50, 0x9a,
    0x55, 0x22, 0x36, 0xa3, 0x48, 0x6e, 0xf0, 0xaf, 0x3a, 0x1e, 0xf8, 0x7d, 0x86, 0x55, 0x83, 0xa9, 0xa7,
    0x4d, 0x8e, 0x5d, 0xb8, 0x1e, 0xdd, 0xdc, 0x87, 0x76, 0x21, 0x1e, 0x6c, 0x70, 0xff,
];

/// Speculos test root CA. Its private key is public knowledge, so accepting it
/// on the device would let any host mint "valid" certificates; hosted builds
/// accept it for local testing only.
#[cfg(not(keyos))]
const ROOT_CA_V3_TEST: [u8; 65] = [
    0x04, 0x37, 0x70, 0x13, 0xa7, 0x8f, 0xde, 0xeb, 0xae, 0xb4, 0x4c, 0x26, 0x25, 0x60, 0xc0, 0x39, 0xb4,
    0x94, 0xbc, 0xfd, 0x89, 0xdc, 0x59, 0xfd, 0x23, 0xcc, 0xca, 0xce, 0x1b, 0x29, 0x52, 0xca, 0xa6, 0x61,
    0x6a, 0x0c, 0xfe, 0x66, 0x83, 0xab, 0x96, 0x39, 0xc6, 0xfc, 0xe4, 0x44, 0xcd, 0xcd, 0x7c, 0x69, 0x86,
    0x8d, 0x27, 0xdc, 0x77, 0xea, 0x66, 0xd8, 0x62, 0x28, 0xae, 0xe5, 0x93, 0x31, 0x72,
];

#[cfg(keyos)]
const ROOT_CA_KEYS: &[[u8; 65]] = &[ROOT_CA_V3_PROD];
#[cfg(not(keyos))]
const ROOT_CA_KEYS: &[[u8; 65]] = &[ROOT_CA_V3_PROD, ROOT_CA_V3_TEST];

/// The last successfully loaded certificate. Lives in RAM only, like on the
/// OS: a fresh app launch starts with no certificate.
#[derive(Clone)]
struct LoadedCert {
    key_usage: u8,
    pk_sign_algo: u8,
    trusted_name: [u8; TRUSTED_NAME_MAXLEN],
    trusted_name_len: usize,
    curve: u8,
    pubkey: [u8; 65],
}

static LOADED: LazyLock<RwLock<Option<LoadedCert>>> = LazyLock::new(|| RwLock::new(None));

/// ABI-correct layout of `cx_ecfp_384_public_key_t` (built with -fshort-enums).
#[repr(C)]
pub struct CxEcfp384PublicKey {
    curve: u8,
    w_len: usize,
    w: [u8; 97],
}

#[derive(Default)]
struct ParsedCert<'a> {
    key_usage: Option<u8>,
    signer_sign_algo: Option<u8>,
    pk_sign_algo: Option<u8>,
    signer_id: Option<u16>,
    trusted_name: [u8; TRUSTED_NAME_MAXLEN],
    trusted_name_len: usize,
    curve: Option<u8>,
    pubkey: Option<[u8; 65]>,
    /// Certificate bytes covered by the signature (everything before its tag).
    payload_len: usize,
    signature: &'a [u8],
}

/// A TLV field that must hold exactly one byte.
fn u8_field(value: &[u8], err: u32) -> Result<u8, u32> {
    match value {
        [v] => Ok(*v),
        _ => Err(err),
    }
}

/// A single-byte TLV field that must stay below `limit` (the enum's UNKNOWN
/// sentinel).
fn bounded_u8_field(value: &[u8], limit: u8, err: u32) -> Result<u8, u32> {
    match u8_field(value, err)? {
        v if v < limit => Ok(v),
        _ => Err(err),
    }
}

fn parse_certificate(cert: &[u8]) -> Result<ParsedCert<'_>, u32> {
    let mut parsed = ParsedCert::default();
    let mut offset = 0usize;

    while offset < cert.len() {
        let tag = cert[offset];
        let len = *cert.get(offset + 1).ok_or(ERR_TAG_MALFORMED)? as usize;
        let value = cert.get(offset + 2..offset + 2 + len).ok_or(ERR_TAG_MALFORMED)?;

        if tag == TAG_SIGNATURE {
            parsed.payload_len = offset;
            parsed.signature = value;
            return Ok(parsed);
        }

        match tag {
            TAG_STRUCTURE_TYPE => {
                if u8_field(value, ERR_STRUCTURE_TYPE)? != STRUCTURE_TYPE_CERTIFICATE {
                    return Err(ERR_STRUCTURE_TYPE);
                }
            }
            TAG_VERSION => {
                bounded_u8_field(value, CERTIFICATE_VERSION_UNKNOWN, ERR_VERSION)?;
            }
            TAG_VALIDITY => {
                // The OS compares this semver against its own version; there is
                // no meaningful OS version here, so only the shape is checked.
                if len != 4 {
                    return Err(ERR_VALIDITY);
                }
            }
            TAG_VALIDITY_INDEX => {
                let index = u32::from_be_bytes(value.try_into().map_err(|_| ERR_VALIDITY_INDEX)?);
                if index <= 1 {
                    return Err(ERR_VALIDITY_INDEX);
                }
            }
            TAG_CHALLENGE => {}
            TAG_SIGNER_KEY_ID => {
                let id = u16::from_be_bytes(value.try_into().map_err(|_| ERR_SIGNER_KEY_ID)?);
                parsed.signer_id = Some(id);
            }
            TAG_SIGN_ALGO_ID => {
                parsed.signer_sign_algo = Some(bounded_u8_field(value, SIGN_ALGO_ID_UNKNOWN, ERR_SIGN_ALGO)?);
            }
            TAG_TIME_VALIDITY => {
                if len != 4 {
                    return Err(ERR_TIME_VALIDITY);
                }
            }
            TAG_TRUSTED_NAME => {
                if len > TRUSTED_NAME_MAXLEN {
                    return Err(ERR_TRUSTED_NAME);
                }
                parsed.trusted_name[..len].copy_from_slice(value);
                parsed.trusted_name_len = len;
            }
            TAG_PUBLIC_KEY_ID => {
                if len != 2 {
                    return Err(ERR_PUBLIC_KEY_ID);
                }
            }
            TAG_PUBLIC_KEY_USAGE => {
                parsed.key_usage = Some(u8_field(value, ERR_KEY_USAGE_FIELD)?);
            }
            TAG_PUBLIC_KEY_CURVE_ID => {
                parsed.curve = Some(u8_field(value, ERR_CURVE_ID)?);
            }
            TAG_COMPRESSED_PUBLIC_KEY => {
                // The curve tag must precede the key, as in the OS parser.
                if parsed.curve != Some(CX_CURVE_SECP256K1) {
                    log::warn!("os_pki: unsupported or missing curve for certificate key");
                    return Err(ERR_CRYPTO);
                }
                let key = keys::secp256k1_decompress(value).ok_or(ERR_CRYPTO)?;
                parsed.pubkey = Some(key);
            }
            TAG_PK_SIGN_ALGO_ID => {
                parsed.pk_sign_algo = Some(bounded_u8_field(value, SIGN_ALGO_ID_UNKNOWN, ERR_PK_SIGN_ALGO)?);
            }
            TAG_TARGET_DEVICE => {
                // Any known device is accepted, as in the OS parser (no match
                // against the emulated model).
                bounded_u8_field(value, TARGET_DEVICE_UNKNOWN, ERR_TARGET_DEVICE)?;
            }
            TAG_DEPTH => {
                u8_field(value, ERR_DEPTH)?;
            }
            _ => return Err(ERR_TAG_MALFORMED),
        }
        offset += 2 + len;
    }
    // No signature tag: nothing to verify.
    Err(ERR_TAG_MALFORMED)
}

/// Whether an algo identifier is one of the ECDSA variants we verify.
/// `payload_digest` maps each of these to its hash; keep the two in step.
fn is_ecdsa_sign_algo(algo: u8) -> bool {
    matches!(algo, SIGN_ALGO_ECDSA_SHA256 | SIGN_ALGO_ECDSA_SHA3_256 | SIGN_ALGO_ECDSA_KECCAK_256)
}

/// SHA-2/SHA-3 digest of the signed payload per the certificate's sign algo.
/// RIPEMD-160 and EdDSA are not supported (the vendor CA signs ECDSA/SHA-256).
fn payload_digest(sign_algo: u8, payload: &[u8]) -> Result<[u8; 32], u32> {
    let digest: [u8; 32] = match sign_algo {
        SIGN_ALGO_ECDSA_SHA256 => Sha256::digest(payload).into(),
        SIGN_ALGO_ECDSA_SHA3_256 => Sha3_256::digest(payload).into(),
        SIGN_ALGO_ECDSA_KECCAK_256 => Keccak256::digest(payload).into(),
        _ => {
            log::warn!("os_pki: unsupported certificate sign algo {sign_algo:#x}");
            return Err(ERR_CRYPTO);
        }
    };
    Ok(digest)
}

/// Parse, verify and store a certificate. On success the loaded state is
/// replaced; on any failure it is cleared, like the OS does.
fn load_certificate_impl(expected_key_usage: u8, cert: &[u8], roots: &[[u8; 65]]) -> Result<LoadedCert, u32> {
    let Ok(mut loaded) = LOADED.write() else {
        return Err(ERR_CRYPTO);
    };
    // The previous certificate stops being valid now, but its key still
    // verifies a chained certificate (one not signed by the root CA).
    let previous = loaded.take();

    let parsed = parse_certificate(cert)?;
    if parsed.key_usage != Some(expected_key_usage) {
        return Err(ERR_KEY_USAGE_MISMATCH);
    }
    let signer_sign_algo = parsed.signer_sign_algo.ok_or(ERR_SIGN_ALGO)?;
    let digest = payload_digest(signer_sign_algo, &cert[..parsed.payload_len])?;

    let verified = if parsed.signer_id == Some(KEY_ID_LEDGER_ROOT_V3) {
        roots.iter().any(|root| keys::secp256k1_verify_der(root, &digest, parsed.signature))
    } else {
        match &previous {
            Some(prev) if prev.curve == CX_CURVE_SECP256K1 => {
                keys::secp256k1_verify_der(&prev.pubkey, &digest, parsed.signature)
            }
            _ => false,
        }
    };
    if !verified {
        return Err(ERR_SIGNATURE);
    }

    let cert = LoadedCert {
        key_usage: expected_key_usage,
        pk_sign_algo: parsed.pk_sign_algo.ok_or(ERR_TAG_MALFORMED)?,
        trusted_name: parsed.trusted_name,
        trusted_name_len: parsed.trusted_name_len,
        curve: parsed.curve.ok_or(ERR_TAG_MALFORMED)?,
        pubkey: parsed.pubkey.ok_or(ERR_TAG_MALFORMED)?,
    };
    *loaded = Some(cert.clone());
    Ok(cert)
}

/// Fill the caller's `cx_ecfp_384_public_key_t` from the loaded certificate.
///
/// # Safety
/// `public_key` must be null or point to a writable `cx_ecfp_384_public_key_t`.
unsafe fn write_public_key(public_key: *mut CxEcfp384PublicKey, cert: &LoadedCert) {
    if public_key.is_null() {
        return;
    }
    let out = &mut *public_key;
    out.curve = cert.curve;
    out.w_len = cert.pubkey.len();
    out.w = [0; 97];
    out.w[..cert.pubkey.len()].copy_from_slice(&cert.pubkey);
}

/// Copy the trusted name to the caller's buffer, when both out params are given.
///
/// # Safety
/// `trusted_name` must be null or point to `TRUSTED_NAME_MAXLEN` writable bytes,
/// and `trusted_name_len` null or writable.
unsafe fn write_trusted_name(trusted_name: *mut u8, trusted_name_len: *mut usize, cert: &LoadedCert) {
    if trusted_name.is_null() || trusted_name_len.is_null() {
        return;
    }
    core::ptr::copy_nonoverlapping(cert.trusted_name.as_ptr(), trusted_name, cert.trusted_name_len);
    *trusted_name_len = cert.trusted_name_len;
}

/// `bolos_err_t os_pki_load_certificate(uint8_t expected_key_usage,
///     uint8_t *certificate, size_t certificate_len, uint8_t *trusted_name,
///     size_t *trusted_name_len, cx_ecfp_384_public_key_t *public_key)`
///
/// # Safety
/// Pointers come from the C SDK: `certificate` must reference
/// `certificate_len` readable bytes; out pointers must be null or writable as
/// described on `write_public_key`/`write_trusted_name`.
#[no_mangle]
pub unsafe extern "C" fn os_pki_load_certificate(
    expected_key_usage: u8,
    certificate: *const u8,
    certificate_len: usize,
    trusted_name: *mut u8,
    trusted_name_len: *mut usize,
    public_key: *mut CxEcfp384PublicKey,
) -> u32 {
    if certificate.is_null() || certificate_len == 0 || certificate_len > MAX_CERTIFICATE_LEN {
        // A rejected load must not leave stale trust state; the OS clears its PKI
        // slot on entry, so mirror that on the path that bypasses the loader.
        if let Ok(mut loaded) = LOADED.write() {
            *loaded = None;
        }
        return ERR_TAG_MALFORMED;
    }
    let cert = core::slice::from_raw_parts(certificate, certificate_len);
    match load_certificate_impl(expected_key_usage, cert, ROOT_CA_KEYS) {
        Ok(cert) => {
            log::info!(
                "os_pki: loaded certificate '{}' (usage {:#04x})",
                String::from_utf8_lossy(&cert.trusted_name[..cert.trusted_name_len]),
                cert.key_usage
            );
            write_trusted_name(trusted_name, trusted_name_len, &cert);
            write_public_key(public_key, &cert);
            SWO_OK
        }
        Err(err) => {
            log::warn!("os_pki: certificate rejected ({err:#06x})");
            err
        }
    }
}

/// `bolos_err_t os_pki_get_info(uint8_t *key_usage, uint8_t *trusted_name,
///     size_t *trusted_name_len, cx_ecfp_384_public_key_t *public_key)`
///
/// # Safety
/// Out pointers must be null or writable as described on
/// `write_public_key`/`write_trusted_name`; `key_usage` null or writable.
#[no_mangle]
pub unsafe extern "C" fn os_pki_get_info(
    key_usage: *mut u8,
    trusted_name: *mut u8,
    trusted_name_len: *mut usize,
    public_key: *mut CxEcfp384PublicKey,
) -> u32 {
    let Ok(loaded) = LOADED.read() else {
        return ERR_CRYPTO;
    };
    let Some(cert) = loaded.as_ref() else {
        return ERR_NO_CERTIFICATE;
    };
    if !key_usage.is_null() {
        *key_usage = cert.key_usage;
    }
    write_trusted_name(trusted_name, trusted_name_len, cert);
    write_public_key(public_key, cert);
    SWO_OK
}

/// `bool os_pki_verify(uint8_t *descriptor_hash, size_t descriptor_hash_len,
///     uint8_t *signature, size_t signature_len)`
///
/// # Safety
/// `descriptor_hash` and `signature` must reference their given lengths.
#[no_mangle]
pub unsafe extern "C" fn os_pki_verify(
    descriptor_hash: *const u8,
    descriptor_hash_len: usize,
    signature: *const u8,
    signature_len: usize,
) -> bool {
    if descriptor_hash.is_null() || descriptor_hash_len != 32 || signature.is_null() || signature_len == 0 {
        return false;
    }
    let Ok(loaded) = LOADED.read() else {
        return false;
    };
    let Some(cert) = loaded.as_ref() else {
        log::warn!("os_pki_verify: no certificate loaded");
        return false;
    };
    if !is_ecdsa_sign_algo(cert.pk_sign_algo) || cert.curve != CX_CURVE_SECP256K1 {
        log::warn!("os_pki_verify: unsupported algo {:#04x} or curve {:#04x}", cert.pk_sign_algo, cert.curve);
        return false;
    }
    let hash = core::slice::from_raw_parts(descriptor_hash, descriptor_hash_len);
    let sig = core::slice::from_raw_parts(signature, signature_len);
    keys::secp256k1_verify_der(&cert.pubkey, hash, sig)
}

#[cfg(test)]
mod tests {
    use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};

    use super::*;

    fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut out = vec![tag, value.len() as u8];
        out.extend_from_slice(value);
        out
    }

    /// Build a certificate for `subject`'s public key, signed by `signer`.
    fn make_cert(signer: &SecretKey, signer_id: u16, subject: &PublicKey, usage: u8, name: &[u8]) -> Vec<u8> {
        let secp = Secp256k1::new();
        let mut cert = Vec::new();
        cert.extend(tlv(TAG_STRUCTURE_TYPE, &[STRUCTURE_TYPE_CERTIFICATE]));
        cert.extend(tlv(TAG_VERSION, &[0x02]));
        cert.extend(tlv(TAG_VALIDITY_INDEX, &5u32.to_be_bytes()));
        cert.extend(tlv(TAG_SIGNER_KEY_ID, &signer_id.to_be_bytes()));
        cert.extend(tlv(TAG_SIGN_ALGO_ID, &[SIGN_ALGO_ECDSA_SHA256]));
        cert.extend(tlv(TAG_TRUSTED_NAME, name));
        cert.extend(tlv(TAG_PUBLIC_KEY_USAGE, &[usage]));
        cert.extend(tlv(TAG_PUBLIC_KEY_CURVE_ID, &[CX_CURVE_SECP256K1]));
        cert.extend(tlv(TAG_COMPRESSED_PUBLIC_KEY, &subject.serialize()));
        cert.extend(tlv(TAG_PK_SIGN_ALGO_ID, &[SIGN_ALGO_ECDSA_SHA256]));
        cert.extend(tlv(TAG_TARGET_DEVICE, &[0x05]));

        let digest: [u8; 32] = Sha256::digest(&cert).into();
        let message = Message::from_digest_slice(&digest).unwrap();
        let sig = secp.sign_ecdsa(&message, signer).serialize_der();
        cert.extend(tlv(TAG_SIGNATURE, &sig));
        cert
    }

    fn key(byte: u8) -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[byte; 32]).unwrap();
        (sk, PublicKey::from_secret_key(&secp, &sk))
    }

    /// One sequential scenario: the loaded-certificate state is process-global,
    /// so the flows share a single test.
    #[test]
    fn certificate_lifecycle() {
        let secp = Secp256k1::new();
        let (root_sk, root_pk) = key(0x11);
        let (partner_sk, partner_pk) = key(0x22);
        let roots = [root_pk.serialize_uncompressed()];
        let usage_tx_simu = 0x0a;

        // SAFETY: os_pki_get_info is the crate's own extern "C" function (callable
        // only via unsafe); passing null for every out-param is explicitly allowed,
        // so this reports just the loaded/not-loaded status.
        let get_info_empty = || unsafe {
            os_pki_get_info(&mut 0, core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut())
        };

        // Nothing loaded yet.
        assert_eq!(get_info_empty(), ERR_NO_CERTIFICATE);

        // Root-signed partner certificate loads.
        let cert = make_cert(&root_sk, KEY_ID_LEDGER_ROOT_V3, &partner_pk, usage_tx_simu, b"Partner");
        assert!(load_certificate_impl(usage_tx_simu, &cert, &roots).is_ok());

        // get_info returns the stored data.
        let mut usage = 0u8;
        let mut name = [0u8; TRUSTED_NAME_MAXLEN];
        let mut name_len = 0usize;
        let mut pubkey = CxEcfp384PublicKey { curve: 0, w_len: 0, w: [0; 97] };
        // SAFETY: os_pki_get_info is extern "C" (unsafe to call); `usage`, `name_len`
        // and `pubkey` are live locals and `name` is a TRUSTED_NAME_MAXLEN buffer, as
        // the out-param contract requires.
        let status = unsafe { os_pki_get_info(&mut usage, name.as_mut_ptr(), &mut name_len, &mut pubkey) };
        assert_eq!(status, SWO_OK);
        assert_eq!(usage, usage_tx_simu);
        assert_eq!(&name[..name_len], b"Partner");
        assert_eq!(pubkey.curve, CX_CURVE_SECP256K1);
        assert_eq!(pubkey.w_len, 65);
        assert_eq!(pubkey.w[..65], partner_pk.serialize_uncompressed());

        // A descriptor signed by the partner key verifies; a tampered hash fails.
        let descriptor_hash: [u8; 32] = Sha256::digest(b"descriptor").into();
        let message = Message::from_digest_slice(&descriptor_hash).unwrap();
        let der = secp.sign_ecdsa(&message, &partner_sk).serialize_der();
        // SAFETY: os_pki_verify is extern "C" (unsafe to call); `hash` is 32 bytes and
        // `der` is valid for `der.len()`, matching the (hash, len, sig, len) contract.
        let verify =
            |hash: &[u8; 32]| unsafe { os_pki_verify(hash.as_ptr(), hash.len(), der.as_ptr(), der.len()) };
        assert!(verify(&descriptor_hash));
        let mut bad_hash = descriptor_hash;
        bad_hash[0] ^= 1;
        assert!(!verify(&bad_hash));

        // A chained certificate verifies against the previously loaded key.
        let (_leaf_sk, leaf_pk) = key(0x33);
        let chained = make_cert(&partner_sk, 0x0009, &leaf_pk, usage_tx_simu, b"Leaf");
        assert!(load_certificate_impl(usage_tx_simu, &chained, &roots).is_ok());

        // Wrong expected usage is rejected and clears the state.
        let cert = make_cert(&root_sk, KEY_ID_LEDGER_ROOT_V3, &partner_pk, usage_tx_simu, b"Partner");
        assert_eq!(load_certificate_impl(0x04, &cert, &roots).err(), Some(ERR_KEY_USAGE_MISMATCH));
        assert_eq!(get_info_empty(), ERR_NO_CERTIFICATE);

        // A tampered certificate fails signature verification.
        let mut tampered = make_cert(&root_sk, KEY_ID_LEDGER_ROOT_V3, &partner_pk, usage_tx_simu, b"Partner");
        let name_pos = tampered.windows(7).position(|w| w == b"Partner").unwrap();
        tampered[name_pos] ^= 1;
        assert_eq!(load_certificate_impl(usage_tx_simu, &tampered, &roots).err(), Some(ERR_SIGNATURE));

        // A certificate signed by an unknown root is rejected.
        let (rogue_sk, _) = key(0x44);
        let rogue = make_cert(&rogue_sk, KEY_ID_LEDGER_ROOT_V3, &partner_pk, usage_tx_simu, b"Partner");
        assert_eq!(load_certificate_impl(usage_tx_simu, &rogue, &roots).err(), Some(ERR_SIGNATURE));

        // The UNKNOWN target-device sentinel is rejected at parse time.
        let mut unknown_target =
            make_cert(&root_sk, KEY_ID_LEDGER_ROOT_V3, &partner_pk, usage_tx_simu, b"Partner");
        let target_pos = unknown_target.windows(3).position(|w| w == [TAG_TARGET_DEVICE, 1, 0x05]).unwrap();
        unknown_target[target_pos + 2] = TARGET_DEVICE_UNKNOWN;
        assert_eq!(
            load_certificate_impl(usage_tx_simu, &unknown_target, &roots).err(),
            Some(ERR_TARGET_DEVICE)
        );

        // A load rejected on argument validation clears any previously loaded
        // certificate, so stale trust state cannot outlive a failed replacement.
        let cert = make_cert(&root_sk, KEY_ID_LEDGER_ROOT_V3, &partner_pk, usage_tx_simu, b"Partner");
        assert!(load_certificate_impl(usage_tx_simu, &cert, &roots).is_ok());
        // SAFETY: os_pki_load_certificate is extern "C" (unsafe to call); the null
        // certificate pointer with length 0 is exactly the rejected-input path under
        // test, and every out-param is null.
        assert_eq!(
            unsafe {
                os_pki_load_certificate(
                    usage_tx_simu,
                    core::ptr::null(),
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                )
            },
            ERR_TAG_MALFORMED
        );
        assert_eq!(get_info_empty(), ERR_NO_CERTIFICATE);
    }
}
