// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    anyhow::{anyhow, Context},
    base64::engine::general_purpose::STANDARD as BASE64,
    base64::Engine,
    serde::Deserialize,
};

use crate::{
    auth::{build_totp_url, get_timestamp_in_seconds, make_totp_auth},
    import_crypto::{decrypt_gcm, GcmDecryptError},
    kdf::{self, ScryptRequest},
    Auth, AuthEditField, CryptoApi,
};

const REQUIRED_AEGIS_PASSWORD_SLOTS: usize = 1;
const AEGIS_EXPORT_VERSION: u32 = 1;
const AEGIS_DB_VERSION: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum AegisError {
    #[error("Aegis password did not match any password slot")]
    PasswordMismatch,
    #[error("Aegis vault contents failed authentication")]
    AuthenticationFailed,
    #[error("{0:?}")]
    GenericError(#[from] anyhow::Error),
}

pub enum ParsedAegisExport {
    Plain(Vec<u8>),
    Encrypted(EncryptedExport),
}

#[derive(Clone)]
pub struct EncryptedExport {
    ciphertext: Vec<u8>,
    db_params: AegisGcmParams,
    password_slots: Vec<AegisSlot>,
}

#[derive(Deserialize)]
struct AegisExportEnvelope {
    version: u32,
    header: AegisHeader,
    db: serde_json::Value,
}

#[derive(Deserialize)]
struct AegisHeader {
    slots: Option<Vec<serde_json::Value>>,
    params: Option<AegisGcmParams>,
}

#[derive(Clone, Deserialize)]
struct AegisSlot {
    key: String,
    key_params: AegisGcmParams,
    n: u32,
    r: u32,
    p: u32,
    salt: String,
}

#[derive(Clone, Deserialize)]
struct AegisGcmParams {
    nonce: String,
    tag: String,
}

#[derive(Deserialize)]
struct AegisEntry {
    #[serde(rename = "type")]
    entry_type: String,
    name: String,
    issuer: String,
    info: serde_json::Value,
}

#[derive(Deserialize)]
struct AegisEntryInfo {
    secret: String,
    algo: String,
    digits: u32,
    period: u64,
}

pub fn parse_export(bytes: &[u8]) -> Result<ParsedAegisExport, AegisError> {
    let envelope: AegisExportEnvelope = serde_json::from_slice(bytes).context("Invalid Aegis export JSON")?;
    if envelope.version != AEGIS_EXPORT_VERSION {
        return Err(anyhow!("Unsupported Aegis export version {}", envelope.version).into());
    }

    match envelope.db {
        serde_json::Value::Object(db) => {
            let db_version = db
                .get("version")
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| anyhow!("Plaintext Aegis db is missing a supported version field"))?;
            if db_version != AEGIS_DB_VERSION {
                return Err(anyhow!("Unsupported Aegis db version {}", db_version).into());
            }
            Ok(ParsedAegisExport::Plain(
                serde_json::to_vec(&db).context("Failed to re-encode plaintext Aegis db")?,
            ))
        }
        serde_json::Value::String(ciphertext) => {
            let slots =
                envelope.header.slots.ok_or_else(|| anyhow!("Encrypted Aegis export has no key slots"))?;
            let password_slots = slots
                .into_iter()
                .filter(|slot| {
                    slot.get("type")
                        .and_then(|value| value.as_u64())
                        .and_then(|value| u32::try_from(value).ok())
                        == Some(1)
                })
                .map(|slot| {
                    serde_json::from_value(slot)
                        .context("Encrypted Aegis export password slot is missing required fields")
                })
                .collect::<Result<Vec<_>, _>>()?;
            if password_slots.len() != REQUIRED_AEGIS_PASSWORD_SLOTS {
                return Err(anyhow!(
                    "Encrypted Aegis export must have exactly {} password key slot (found {})",
                    REQUIRED_AEGIS_PASSWORD_SLOTS,
                    password_slots.len()
                )
                .into());
            }
            let db_params = envelope
                .header
                .params
                .ok_or_else(|| anyhow!("Encrypted Aegis export is missing header parameters"))?;
            let ciphertext = BASE64.decode(ciphertext).context("Aegis DB base64 decode failed")?;

            Ok(ParsedAegisExport::Encrypted(EncryptedExport { ciphertext, db_params, password_slots }))
        }
        _ => Err(anyhow!("Invalid Aegis export structure").into()),
    }
}

pub fn ingest_uri_export(bytes: &[u8]) -> Result<Vec<Auth>, AegisError> {
    let text = std::str::from_utf8(bytes).context("Invalid UTF-8 in Aegis URI export")?;
    let mut entries = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        entries.push(parse_uri_entry(line)?);
    }

    if entries.is_empty() {
        return Err(anyhow!("Aegis URI export contains no importable entries").into());
    }

    Ok(entries)
}

pub fn probe_uri_export(bytes: &[u8]) -> Result<(), AegisError> {
    let text = std::str::from_utf8(bytes).context("Invalid UTF-8 in Aegis URI export")?;
    let mut saw_entry = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if !line.starts_with("otpauth://") {
            return Err(anyhow!("Invalid Aegis URI export line").into());
        }

        saw_entry = true;
    }

    if !saw_entry {
        return Err(anyhow!("Aegis URI export contains no importable entries").into());
    }

    Ok(())
}

pub async fn decrypt_export(
    crypto: &CryptoApi,
    export: &EncryptedExport,
    password: &str,
) -> Result<Vec<u8>, AegisError> {
    let mut db_key: Option<Vec<u8>> = None;

    for slot in export.password_slots.iter() {
        let salt = decode_hex(&slot.salt)?;

        let derived_key = kdf::derive_scrypt(ScryptRequest {
            password: password.as_bytes().to_vec(),
            salt,
            n: slot.n,
            r: slot.r,
            p: slot.p,
            out_len: 32,
        })
        .await
        .map_err(|_| anyhow!("Aegis key derivation failed"))?;

        let wrapped_key = decode_hex(&slot.key)?;
        let slot_nonce = decode_external_nonce(&slot.key_params.nonce)?;
        let slot_tag = decode_external_tag(&slot.key_params.tag)?;

        match decrypt_gcm(crypto, derived_key.as_slice(), slot_nonce, slot_tag, &wrapped_key, None) {
            Ok(key) => {
                db_key = Some(key);
                break;
            }
            Err(GcmDecryptError::AuthenticationFailed) => {
                continue;
            }
            Err(GcmDecryptError::Operation(error)) => return Err(error.into()),
        }
    }

    let db_key = db_key.ok_or(AegisError::PasswordMismatch)?;

    let db_nonce = decode_external_nonce(&export.db_params.nonce)?;
    let db_tag = decode_external_tag(&export.db_params.tag)?;
    let plaintext = decrypt_gcm(crypto, db_key.as_slice(), db_nonce, db_tag, &export.ciphertext, None)
        .map_err(|error| match error {
            GcmDecryptError::AuthenticationFailed => AegisError::AuthenticationFailed,
            GcmDecryptError::Operation(error) => error.into(),
        })?;

    Ok(plaintext)
}

pub fn ingest_plaintext_db(bytes: &[u8]) -> Result<Vec<Auth>, AegisError> {
    #[derive(Deserialize)]
    struct PlainDb {
        version: u32,
        entries: Vec<AegisEntry>,
    }

    let db: PlainDb = serde_json::from_slice(bytes).context("Invalid plaintext Aegis db")?;
    if db.version != AEGIS_DB_VERSION {
        return Err(anyhow!("Unsupported Aegis db version {}", db.version).into());
    }
    let mut entries = Vec::new();

    for entry in db.entries.into_iter() {
        if entry.entry_type != "totp" {
            continue;
        }
        entries.push(parse_plaintext_entry(entry)?);
    }

    if entries.is_empty() {
        return Err(anyhow!("Aegis export contains no supported TOTP entries").into());
    }

    Ok(entries)
}

fn parse_uri_entry(line: &str) -> Result<Auth, AegisError> {
    if !line.starts_with("otpauth://") {
        return Err(anyhow!("Invalid Aegis URI export line").into());
    }

    let mut auth = Auth::new(line.to_owned(), get_timestamp_in_seconds()).map_err(anyhow::Error::new)?;
    if auth.get_issuer().is_empty() {
        auth.edit(AuthEditField::Label(auth.get_account().to_string())).map_err(anyhow::Error::new)?;
    }
    Ok(auth)
}

fn parse_plaintext_entry(entry: AegisEntry) -> Result<Auth, AegisError> {
    let info: AegisEntryInfo =
        serde_json::from_value(entry.info).context("Aegis TOTP entry is missing required info fields")?;

    let issuer = (!entry.issuer.is_empty()).then_some(entry.issuer.as_str());
    let url = build_totp_url(&info.secret, &entry.name, issuer, &info.algo, info.digits, info.period)?;
    make_totp_auth(&url, issuer).map_err(AegisError::from)
}

fn decode_hex(input: &str) -> Result<Vec<u8>, AegisError> {
    if (input.len() % 2) != 0 {
        return Err(anyhow!("Invalid hex data in Aegis export").into());
    }

    input
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| {
            let high = decode_hex_nibble(chunk[0])?;
            let low = decode_hex_nibble(chunk[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn decode_fixed_hex<const N: usize>(input: &str) -> Result<[u8; N], AegisError> {
    let bytes = decode_hex(input)?;
    bytes.try_into().map_err(|_| anyhow!("Invalid fixed-width hex data in Aegis export").into())
}

fn decode_external_nonce(input: &str) -> Result<[u8; 12], AegisError> { decode_fixed_hex::<12>(input) }

fn decode_external_tag(input: &str) -> Result<[u8; 16], AegisError> { decode_fixed_hex::<16>(input) }

fn decode_hex_nibble(value: u8) -> Result<u8, AegisError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(anyhow!("Invalid hex data in Aegis export").into()),
    }
}

#[cfg(test)]
mod tests {
    use ordered_table::SortableCard;

    use super::*;
    use crate::tr;

    #[test]
    fn test_parse_plain_export() {
        let export = br#"{
            "version": 1,
            "header": { "slots": null, "params": null },
            "db": {
                "version": 3,
                "entries": [{
                    "type": "totp",
                    "uuid": "x",
                    "name": "alice@example.com",
                    "issuer": "Example",
                    "note": "",
                    "favorite": false,
                    "icon": null,
                    "info": {
                        "secret": "JBSWY3DPEHPK3PXP",
                        "algo": "SHA1",
                        "digits": 6,
                        "period": 30
                    },
                    "groups": []
                }],
                "groups": [],
                "icons_optimized": true
            }
        }"#;

        let ParsedAegisExport::Plain(db) = parse_export(export).unwrap() else {
            panic!("expected plain export");
        };
        let entries = ingest_plaintext_db(db.as_slice()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "Example");
        assert_eq!(entries[0].get_account(), "alice@example.com");
    }

    #[test]
    fn test_parse_encrypted_export_shape() {
        let export = br#"{
            "version": 1,
            "header": {
                "slots": [{
                    "type": 1,
                    "uuid": "x",
                    "key": "00112233445566778899aabbccddeeff",
                    "key_params": {
                        "nonce": "00112233445566778899aabb",
                        "tag": "00112233445566778899aabbccddeeff"
                    },
                    "n": 32768,
                    "r": 8,
                    "p": 1,
                    "salt": "00112233445566778899aabbccddeeff",
                    "repaired": false,
                    "is_backup": false
                }],
                "params": {
                    "nonce": "00112233445566778899aabb",
                    "tag": "00112233445566778899aabbccddeeff"
                }
            },
            "db": "AA=="
        }"#;

        let ParsedAegisExport::Encrypted(parsed) = parse_export(export).unwrap() else {
            panic!("expected encrypted export");
        };
        assert_eq!(parsed.ciphertext, vec![0]);
        assert_eq!(parsed.password_slots.len(), 1);
        assert_eq!(parsed.password_slots[0].n, 32768);
    }

    #[test]
    fn test_parse_encrypted_export_ignores_non_password_slots() {
        let export = br#"{
            "version": 1,
            "header": {
                "slots": [
                    {
                        "type": 2,
                        "uuid": "biometric-only"
                    },
                    {
                        "type": 1,
                        "uuid": "password-slot",
                        "key": "00112233445566778899aabbccddeeff",
                        "key_params": {
                            "nonce": "00112233445566778899aabb",
                            "tag": "00112233445566778899aabbccddeeff"
                        },
                        "n": 32768,
                        "r": 8,
                        "p": 1,
                        "salt": "00112233445566778899aabbccddeeff",
                        "repaired": false,
                        "is_backup": false
                    }
                ],
                "params": {
                    "nonce": "00112233445566778899aabb",
                    "tag": "00112233445566778899aabbccddeeff"
                }
            },
            "db": "AA=="
        }"#;

        let ParsedAegisExport::Encrypted(parsed) = parse_export(export).unwrap() else {
            panic!("expected encrypted export");
        };
        assert_eq!(parsed.password_slots.len(), 1);
        assert_eq!(parsed.password_slots[0].n, 32768);
    }

    #[test]
    fn test_parse_encrypted_export_rejects_wrong_slot_type() {
        let export = br#"{
            "version": 1,
            "header": {
                "slots": [{
                    "type": 2,
                    "uuid": "biometric-only"
                }],
                "params": {
                    "nonce": "00112233445566778899aabb",
                    "tag": "00112233445566778899aabbccddeeff"
                }
            },
            "db": "AA=="
        }"#;

        let error = match parse_export(export) {
            Ok(_) => panic!("expected parse_export to fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("exactly 1 password key slot"));
    }

    #[test]
    fn test_parse_encrypted_export_rejects_too_many_password_slots() {
        let mut slots = String::new();
        for i in 0..=REQUIRED_AEGIS_PASSWORD_SLOTS {
            if i > 0 {
                slots.push(',');
            }
            slots.push_str(
                r#"{
                    "type": 1,
                    "uuid": "password-slot",
                    "key": "00112233445566778899aabbccddeeff",
                    "key_params": {
                        "nonce": "00112233445566778899aabb",
                        "tag": "00112233445566778899aabbccddeeff"
                    },
                    "n": 32768,
                    "r": 8,
                    "p": 1,
                    "salt": "00112233445566778899aabbccddeeff",
                    "repaired": false,
                    "is_backup": false
                }"#,
            );
        }

        let export = format!(
            r#"{{
                "version": 1,
                "header": {{
                    "slots": [{slots}],
                    "params": {{
                        "nonce": "00112233445566778899aabb",
                        "tag": "00112233445566778899aabbccddeeff"
                    }}
                }},
                "db": "AA=="
            }}"#
        );

        let error = match parse_export(export.as_bytes()) {
            Ok(_) => panic!("expected parse_export to fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("exactly 1 password key slot"));
    }

    #[test]
    fn test_parse_export_rejects_unsupported_top_level_version() {
        let export = br#"{
            "version": 2,
            "header": { "slots": null, "params": null },
            "db": {
                "version": 3,
                "entries": [],
                "groups": [],
                "icons_optimized": true
            }
        }"#;

        let error = match parse_export(export) {
            Ok(_) => panic!("expected parse_export to fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("Unsupported Aegis export version 2"));
    }

    #[test]
    fn test_parse_export_rejects_unsupported_plain_db_version() {
        let export = br#"{
            "version": 1,
            "header": { "slots": null, "params": null },
            "db": {
                "version": 4,
                "entries": [],
                "groups": [],
                "icons_optimized": true
            }
        }"#;

        let error = match parse_export(export) {
            Ok(_) => panic!("expected parse_export to fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("Unsupported Aegis db version 4"));
    }

    #[test]
    fn test_ingest_plaintext_db_skips_non_totp_entries_with_different_info_shape() {
        let db = br#"{
            "version": 3,
            "entries": [
                {
                    "type": "hotp",
                    "uuid": "hotp",
                    "name": "counter@example.com",
                    "issuer": "Example",
                    "info": {
                        "secret": "JBSWY3DPEHPK3PXP",
                        "algo": "SHA1",
                        "digits": 6,
                        "counter": 1
                    }
                },
                {
                    "type": "totp",
                    "uuid": "totp",
                    "name": "alice@example.com",
                    "issuer": "Example",
                    "info": {
                        "secret": "JBSWY3DPEHPK3PXP",
                        "algo": "SHA1",
                        "digits": 6,
                        "period": 30
                    }
                }
            ]
        }"#;

        let entries = ingest_plaintext_db(db).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "Example");
        assert_eq!(entries[0].get_account(), "alice@example.com");
    }

    #[test]
    fn test_ingest_plaintext_db_uses_name_as_display_label_when_issuer_is_missing() {
        let db = br#"{
            "version": 3,
            "entries": [
                {
                    "type": "totp",
                    "uuid": "totp",
                    "name": "alice@example.com",
                    "issuer": "",
                    "info": {
                        "secret": "JBSWY3DPEHPK3PXP",
                        "algo": "SHA1",
                        "digits": 6,
                        "period": 30
                    }
                }
            ]
        }"#;

        let entries = ingest_plaintext_db(db).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "alice@example.com");
        assert_eq!(entries[0].get_account(), "alice@example.com");
        assert_eq!(entries[0].get_issuer(), "");
    }

    #[test]
    fn test_ingest_plaintext_db_keeps_blank_name_entries_importable() {
        let db = br#"{
            "version": 3,
            "entries": [
                {
                    "type": "totp",
                    "uuid": "blank-name",
                    "name": "",
                    "issuer": "",
                    "info": {
                        "secret": "JBSWY3DPEHPK3PXP",
                        "algo": "SHA1",
                        "digits": 6,
                        "period": 30
                    }
                }
            ]
        }"#;

        let entries = ingest_plaintext_db(db).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), tr::lookup_id(crate::TrId::MainImportNoName));
        assert_eq!(entries[0].get_account(), tr::lookup_id(crate::TrId::MainImportNoName));
        assert_eq!(entries[0].get_issuer(), "");
    }

    #[test]
    fn test_ingest_plaintext_db_rejects_malformed_entries() {
        let db = br#"{
            "version": 3,
            "entries": [
                {
                    "type": "totp",
                    "uuid": "missing-name",
                    "issuer": "Broken",
                    "info": {
                        "secret": "JBSWY3DPEHPK3PXP",
                        "algo": "SHA1",
                        "digits": 6,
                        "period": 30
                    }
                },
                {
                    "type": "totp",
                    "uuid": "valid",
                    "name": "alice@example.com",
                    "issuer": "Example",
                    "info": {
                        "secret": "JBSWY3DPEHPK3PXP",
                        "algo": "SHA1",
                        "digits": 6,
                        "period": 30
                    }
                }
            ]
        }"#;

        let error = ingest_plaintext_db(db).unwrap_err().to_string();
        assert!(error.contains("missing field"));
    }

    #[test]
    fn test_ingest_uri_export_rejects_malformed_lines() {
        let export = br#"
not-an-otpauth-line
otpauth://totp/Example:alice%40example.com?issuer=Example&secret=JBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30
otpauth://totp/missing-secret?issuer=Broken
"#;

        let error = ingest_uri_export(export).unwrap_err().to_string();
        assert!(error.contains("Invalid Aegis URI export line"));
    }

    #[test]
    fn test_ingest_uri_export_uses_account_as_label_when_issuer_is_missing() {
        let export =
            br#"otpauth://totp/alice%40example.com?secret=JBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30"#;

        let entries = ingest_uri_export(export).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "alice@example.com");
        assert_eq!(entries[0].get_account(), "alice@example.com");
        assert_eq!(entries[0].get_issuer(), "");
    }

    #[test]
    fn test_probe_uri_export_accepts_uri_shape_without_parsing_secret() {
        let export = br#"
otpauth://totp/Broken?issuer=Broken
otpauth://totp/Example:alice%40example.com?issuer=Example&secret=JBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30
"#;

        probe_uri_export(export).unwrap();
    }
}
