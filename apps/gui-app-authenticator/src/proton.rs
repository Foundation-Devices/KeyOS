// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        kdf::{self, Argon2idRequest},
        tr, CryptoApi,
    },
    anyhow::{bail, Context},
    base64::{engine::general_purpose::STANDARD as BASE64, Engine},
    csv::ReaderBuilder,
    pgp::{composed::Message as PgpMessage, errors::Error as PgpError},
    serde::Deserialize,
    std::{
        collections::{BTreeMap, HashMap},
        io::{Cursor, Read},
    },
    url::Url,
    zip::{CompressionMethod, ZipArchive},
};

use crate::{
    auth::{build_totp_url, make_totp_auth},
    import_crypto::{decrypt_gcm, GcmDecryptError},
    Auth, TrId,
};

pub const ZIP_JSON_ENTRY: &str = "Proton Pass/data.json";
pub const ZIP_PGP_ENTRY: &str = "Proton Pass/data.pgp";
const LEGACY_VERSION: u32 = 1;
const LEGACY_ARGON2_MEMORY_KIB: u32 = 19_456;
const LEGACY_ARGON2_ITERATIONS: u32 = 2;
const LEGACY_ARGON2_LANES: u32 = 1;
const LEGACY_SALT_LEN: usize = 16;
const LEGACY_GCM_NONCE_LEN: usize = 12;
const LEGACY_GCM_TAG_LEN: usize = 16;
const LEGACY_AES256_KEY_LEN: usize = 32;
const PROTON_AUTHENTICATOR_PASSWORD_EXPORT_AAD: &[u8] = b"proton.authenticator.export.v1";

#[derive(Debug, thiserror::Error)]
pub enum ProtonError {
    #[error("Proton import password did not decrypt the file")]
    PasswordMismatch,
    #[error(transparent)]
    Generic(#[from] anyhow::Error),
}

#[derive(Deserialize)]
struct ProtonExport {
    vaults: BTreeMap<String, ProtonVault>,
}

#[derive(Deserialize)]
struct ProtonVault {
    items: Vec<ProtonItem>,
}

#[derive(Deserialize)]
struct ProtonItem {
    data: ProtonItemData,
}

#[derive(Deserialize)]
struct ProtonItemData {
    metadata: ProtonMetadata,
    #[serde(rename = "type")]
    item_type: String,
    content: ProtonContent,
}

#[derive(Deserialize)]
struct ProtonMetadata {
    name: Option<String>,
}

#[derive(Deserialize)]
struct ProtonContent {
    #[serde(rename = "totpUri")]
    #[serde(default)]
    totp_uri: String,
}

#[derive(Deserialize)]
struct ProtonCsvItem {
    #[serde(rename = "type")]
    item_type: String,
    name: String,
    #[serde(rename = "totp")]
    totp_uri: String,
}

#[derive(Clone, Deserialize)]
pub struct ProtonAuthenticatorEncryptedExport {
    version: u32,
    salt: String,
    content: String,
}

#[derive(Deserialize)]
struct ProtonAuthenticatorPlainExport {
    version: u32,
    entries: Vec<ProtonAuthenticatorEntry>,
}

#[derive(Debug, Deserialize)]
struct ProtonAuthenticatorPlainProbe {
    version: u32,
    #[serde(rename = "entries")]
    _entries: serde::de::IgnoredAny,
}

#[derive(Deserialize)]
struct ProtonAuthenticatorEntry {
    content: ProtonAuthenticatorEntryContent,
}

#[derive(Deserialize)]
struct ProtonAuthenticatorEntryContent {
    uri: String,
    entry_type: String,
    name: Option<String>,
}

pub enum ParsedAuthenticatorExport {
    Plain,
    Encrypted(ProtonAuthenticatorEncryptedExport),
}

pub fn extract_zip_entry(bytes: &[u8], entry_name: &str) -> anyhow::Result<Vec<u8>> {
    let reader = Cursor::new(bytes);
    let mut archive = ZipArchive::new(reader).context("Invalid ZIP archive")?;
    if archive.index_for_name(entry_name).is_none() {
        bail!("ZIP entry not found: {entry_name}");
    }

    let mut file = archive.by_name(entry_name).context("Failed to open ZIP entry")?;
    if file.compression() != CompressionMethod::Stored {
        bail!("Unsupported ZIP compression for {entry_name}: only stored entries are supported");
    }
    let capacity = usize::try_from(file.size()).context("ZIP entry too large to read")?.min(bytes.len());
    let mut data = Vec::with_capacity(capacity);
    file.read_to_end(&mut data).context("Failed to read ZIP entry")?;
    Ok(data)
}

pub fn parse_authenticator_export(bytes: &[u8]) -> anyhow::Result<ParsedAuthenticatorExport> {
    if let Ok(export) = serde_json::from_slice::<ProtonAuthenticatorEncryptedExport>(bytes) {
        if export.version == LEGACY_VERSION {
            return Ok(ParsedAuthenticatorExport::Encrypted(export));
        }
    }

    let export: ProtonAuthenticatorPlainProbe =
        crate::from_import_json(bytes, "Invalid Proton Authenticator export JSON")?;
    if export.version != LEGACY_VERSION {
        return Err(anyhow::anyhow!("Unsupported Proton Authenticator export version").into());
    }

    Ok(ParsedAuthenticatorExport::Plain)
}

pub fn decrypt_pgp_export(bytes: &[u8], password: &str) -> Result<Vec<u8>, ProtonError> {
    let armored = std::str::from_utf8(bytes).context("Invalid Proton PGP export encoding")?;
    let (message, _headers) = PgpMessage::from_armor(armored.as_bytes()).map_err(map_pgp_error)?;
    let mut decrypted = message.decrypt_with_password(&password.into()).map_err(map_pgp_error)?;
    if decrypted.is_compressed() {
        return Err(ProtonError::Generic(anyhow::anyhow!("Compressed OpenPGP payloads are unsupported")));
    }

    decrypted.as_data_vec().map_err(map_pgp_io_error)
}

pub async fn decrypt_authenticator_export(
    crypto: &CryptoApi,
    export: &ProtonAuthenticatorEncryptedExport,
    password: &str,
) -> Result<Vec<u8>, ProtonError> {
    if export.version != LEGACY_VERSION {
        return Err(anyhow::anyhow!("Unsupported Proton Authenticator export version").into());
    }

    let salt = BASE64.decode(export.salt.as_bytes()).context("Invalid Proton Authenticator salt encoding")?;
    if salt.len() != LEGACY_SALT_LEN {
        return Err(anyhow::anyhow!("Invalid Proton Authenticator salt length").into());
    }

    let content =
        BASE64.decode(export.content.as_bytes()).context("Invalid Proton Authenticator content encoding")?;
    if content.len() <= LEGACY_GCM_NONCE_LEN + LEGACY_GCM_TAG_LEN {
        return Err(anyhow::anyhow!("Invalid Proton Authenticator content length").into());
    }

    let key = derive_authenticator_key(password.as_bytes(), salt.as_slice()).await?;

    let nonce: [u8; LEGACY_GCM_NONCE_LEN] = content[..LEGACY_GCM_NONCE_LEN].try_into().unwrap();
    let ciphertext_end = content.len() - LEGACY_GCM_TAG_LEN;
    let expected_tag: [u8; LEGACY_GCM_TAG_LEN] = content[ciphertext_end..].try_into().unwrap();
    let ciphertext = &content[LEGACY_GCM_NONCE_LEN..ciphertext_end];

    let plaintext = decrypt_gcm(
        crypto,
        key.as_slice(),
        nonce,
        expected_tag,
        ciphertext,
        Some(PROTON_AUTHENTICATOR_PASSWORD_EXPORT_AAD),
    )
    .map_err(|error| match error {
        GcmDecryptError::AuthenticationFailed => ProtonError::PasswordMismatch,
        GcmDecryptError::Operation(error) => error.into(),
    })?;
    Ok(plaintext)
}

pub fn ingest_json_export(bytes: &[u8]) -> anyhow::Result<Vec<Auth>> {
    let export: ProtonExport = crate::from_import_json(bytes, "Invalid Proton export JSON")?;
    let mut entries = Vec::new();

    for vault in export.vaults.into_values() {
        for item in vault.items {
            if item.data.item_type != "login" || item.data.content.totp_uri.is_empty() {
                continue;
            }

            let name = item.data.metadata.name.as_deref();
            let totp_uri = normalize_proton_pass_totp_uri(item.data.content.totp_uri.as_str(), name)?;
            entries.push(make_totp_auth(totp_uri.as_str(), name)?);
        }
    }

    if entries.is_empty() {
        bail!("Proton export contains no importable TOTP entries");
    }

    Ok(entries)
}

pub fn ingest_authenticator_plain_export(bytes: &[u8]) -> anyhow::Result<Vec<Auth>> {
    let export: ProtonAuthenticatorPlainExport =
        crate::from_import_json(bytes, "Invalid Proton Authenticator export JSON")?;
    if export.version != LEGACY_VERSION {
        bail!("Unsupported Proton Authenticator export version");
    }

    let mut entries = Vec::new();

    for entry in export.entries {
        if entry.content.entry_type != "Totp" || entry.content.uri.is_empty() {
            continue;
        }

        entries.push(parse_authenticator_entry(entry)?);
    }

    if entries.is_empty() {
        bail!("Proton Authenticator export contains no importable TOTP entries");
    }

    Ok(entries)
}

pub fn probe_csv_export(bytes: &[u8]) -> anyhow::Result<()> {
    let mut reader =
        ReaderBuilder::new().has_headers(true).trim(csv::Trim::All).from_reader(Cursor::new(bytes));
    let headers = reader.byte_headers().map_err(|_| anyhow::anyhow!("Invalid Proton CSV headers"))?.clone();

    let type_index = headers
        .iter()
        .position(|field| field == b"type")
        .context("Proton CSV export is missing type column")?;
    let totp_index = headers
        .iter()
        .position(|field| field == b"totp")
        .context("Proton CSV export is missing totp column")?;

    for row in reader.byte_records() {
        let row = row.map_err(|_| anyhow::anyhow!("Invalid Proton CSV row"))?;
        let item_type = row.get(type_index).unwrap_or_default();
        let totp_uri = row.get(totp_index).unwrap_or_default();
        if item_type == b"login" && !totp_uri.is_empty() {
            return Ok(());
        }
    }

    bail!("Proton CSV export contains no importable TOTP entries");
}

pub fn ingest_csv_export(bytes: &[u8]) -> anyhow::Result<Vec<Auth>> {
    let mut reader =
        ReaderBuilder::new().has_headers(true).trim(csv::Trim::All).from_reader(Cursor::new(bytes));
    let mut entries = Vec::new();

    for row in reader.deserialize::<ProtonCsvItem>() {
        if let Some(import_entry) = parse_csv_entry(row)? {
            entries.push(import_entry);
        }
    }

    if entries.is_empty() {
        bail!("Proton CSV export contains no importable TOTP entries");
    }

    Ok(entries)
}

fn parse_authenticator_entry(entry: ProtonAuthenticatorEntry) -> anyhow::Result<Auth> {
    let normalized_uri = normalize_authenticator_totp_uri(entry.content.uri.as_str())?;
    make_totp_auth(normalized_uri.as_str(), entry.content.name.as_deref())
}

fn parse_csv_entry(row: Result<ProtonCsvItem, csv::Error>) -> anyhow::Result<Option<Auth>> {
    let row = row.map_err(|_| anyhow::anyhow!("Invalid Proton CSV row"))?;
    if row.item_type != "login" || row.totp_uri.is_empty() {
        return Ok(None);
    }

    let name = (!row.name.is_empty()).then_some(row.name.as_str());
    let totp_uri = normalize_proton_pass_totp_uri(row.totp_uri.as_str(), name)?;
    make_totp_auth(totp_uri.as_str(), name).map(Some)
}

async fn derive_authenticator_key(
    password: &[u8],
    salt: &[u8],
) -> anyhow::Result<[u8; LEGACY_AES256_KEY_LEN]> {
    let derived = kdf::derive_argon2id(Argon2idRequest {
        password: password.to_vec(),
        salt: salt.to_vec(),
        secret: Vec::new(),
        associated_data: Vec::new(),
        mem_kib: LEGACY_ARGON2_MEMORY_KIB,
        iterations: LEGACY_ARGON2_ITERATIONS,
        parallelism: LEGACY_ARGON2_LANES,
        out_len: LEGACY_AES256_KEY_LEN,
    })
    .await
    .map_err(|error| anyhow::anyhow!("Proton Authenticator Argon2 key derivation failed: {}", error))?;

    let derived_bytes: &[u8; LEGACY_AES256_KEY_LEN] = derived
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid Proton Authenticator derived key length"))?;
    let mut key = [0; LEGACY_AES256_KEY_LEN];
    key.copy_from_slice(derived_bytes);
    Ok(key)
}

fn normalize_authenticator_totp_uri(uri: &str) -> anyhow::Result<String> {
    let parsed = Url::parse(uri).context("Invalid Proton Authenticator TOTP URI")?;
    if parsed.scheme() != "otpauth" || parsed.host_str() != Some("totp") {
        bail!("Proton Authenticator entry must use an otpauth://totp/ URI");
    }
    let params = parsed.query_pairs().collect::<HashMap<_, _>>();

    params
        .get("secret")
        .map(|value| value.as_ref())
        .filter(|value| !value.is_empty())
        .context("Proton Authenticator TOTP entry is missing a secret")?;

    Ok(parsed.into())
}

fn normalize_proton_pass_totp_uri(totp_uri: &str, name: Option<&str>) -> anyhow::Result<String> {
    if totp_uri.starts_with("otpauth://") {
        return Ok(totp_uri.to_string());
    }

    let account =
        name.filter(|name| !name.is_empty()).unwrap_or_else(|| tr::lookup_id(TrId::MainImportNoName));
    build_totp_url(totp_uri, account, None, "SHA1", 6, 30)
}

fn classify_pgp_error(error: &PgpError) -> Option<ProtonError> {
    match error {
        PgpError::MissingKey
        | PgpError::ChecksumMissmatch { .. }
        | PgpError::MdcError
        | PgpError::Aead { .. } => Some(ProtonError::PasswordMismatch),
        _ => None,
    }
}

fn map_pgp_error(error: PgpError) -> ProtonError {
    classify_pgp_error(&error).unwrap_or_else(|| ProtonError::Generic(anyhow::Error::new(error)))
}

fn map_pgp_io_error(error: std::io::Error) -> ProtonError {
    if let Some(pgp_error) = error.get_ref().and_then(|source| source.downcast_ref::<PgpError>()) {
        if let Some(mapped) = classify_pgp_error(pgp_error) {
            return mapped;
        }
    }

    ProtonError::Generic(error.into())
}

#[cfg(test)]
mod tests {
    use ordered_table::SortableCard;

    use super::*;

    const PROTON_PASS_ENCRYPTED_ZIP: &[u8] = include_bytes!("../test-fixtures/proton-pass-encrypted.zip");
    const PROTON_PASS_PLAIN_ZIP: &[u8] = include_bytes!("../test-fixtures/proton-pass-plain.zip");
    const PROTON_AUTHENTICATOR_PLAIN_EXPORT: &str = "{\"version\":1,\"entries\":[{\"id\":\"bc5d37dd-ee98-4e5f-8147-2d8ada5c0017\",\"content\":{\"uri\":\"otpauth://totp/ftsh%3Adfthiluaydw:ad?secret=MSPC56X6TBIVGUMH7R2K2YREQSBKM7QR&issuer=ftsh%3Adfthiluaydw&algorithm=SHA1&digits=6&period=30\",\"entry_type\":\"Totp\",\"name\":\"tysrdstdw:ad\"},\"note\":null},{\"id\":\"1e0d4f4c-83fa-4025-85bd-0a45d266b5d6\",\"content\":{\"uri\":\"otpauth://totp/zdrgzdrg?secret=SCGHMHH6GFBRQ3OTHCJCZCZ4S2SAFCQJ&issuer=zdrgzdrg&algorithm=SHA256&digits=6&period=30\",\"entry_type\":\"Totp\",\"name\":\"zdrgzdrg\"},\"note\":null}]}";
    const PROTON_AUTHENTICATOR_ENCRYPTED_EXPORT: &str =
        "{\"version\":1,\"salt\":\"4fBBBCh3ybTsAQUEQabcbQ==\",\"content\":\"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\"}";
    const PROTON_CSV_EXPORT: &str = "type,name,url,email,username,password,note,totp,createTime,modifyTime,vault\nlogin,Example 2FA,,,,,,otpauth://totp/Example:alice%40google.com?issuer=Example&secret=ZBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30,1781999631,1781999631,Personal\nlogin,Example 2FA 2,,,,,,otpauth://totp/Example:alice%40google.com?issuer=Example&secret=YBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30,1781999665,1781999665,Personal\n";
    const PROTON_JSON_EXPORT_WITH_MISSING_TOTP_URI: &str = r#"{
        "vaults": {
            "vault-1": {
                "items": [
                    {
                        "data": {
                            "metadata": { "name": "No TOTP" },
                            "type": "login",
                            "content": {}
                        }
                    },
                    {
                        "data": {
                            "metadata": { "name": "Example 2FA" },
                            "type": "login",
                            "content": {
                                "totpUri": "otpauth://totp/Example:alice%40google.com?issuer=Example&secret=ZBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30"
                            }
                        }
                    }
                ]
            }
        }
    }"#;

    #[test]
    fn parse_legacy_export_detects_plain() {
        let parsed = parse_authenticator_export(PROTON_AUTHENTICATOR_PLAIN_EXPORT.as_bytes()).unwrap();
        assert!(matches!(parsed, ParsedAuthenticatorExport::Plain));
    }

    #[test]
    fn parse_legacy_export_detects_encrypted() {
        let parsed = parse_authenticator_export(PROTON_AUTHENTICATOR_ENCRYPTED_EXPORT.as_bytes()).unwrap();
        assert!(matches!(parsed, ParsedAuthenticatorExport::Encrypted(_)));
    }

    #[test]
    fn decrypt_pgp_export_matches_plain_zip_fixture() {
        let encrypted = extract_zip_entry(PROTON_PASS_ENCRYPTED_ZIP, ZIP_PGP_ENTRY).unwrap();
        let expected = extract_zip_entry(PROTON_PASS_PLAIN_ZIP, ZIP_JSON_ENTRY).unwrap();

        let decrypted = decrypt_pgp_export(encrypted.as_slice(), "123456").unwrap();

        assert_eq!(decrypted.as_slice(), expected.as_slice());
    }

    #[test]
    fn decrypt_pgp_export_wrong_password_is_password_mismatch() {
        let encrypted = extract_zip_entry(PROTON_PASS_ENCRYPTED_ZIP, ZIP_PGP_ENTRY).unwrap();

        let error = decrypt_pgp_export(encrypted.as_slice(), "wrong-password").unwrap_err();

        assert!(matches!(error, ProtonError::PasswordMismatch));
    }

    #[test]
    fn ingest_legacy_plain_export_imports_totp_rows() {
        let entries =
            ingest_authenticator_plain_export(PROTON_AUTHENTICATOR_PLAIN_EXPORT.as_bytes()).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].get_label(), "tysrdstdw:ad");
        assert_eq!(entries[0].get_issuer(), "ftsh:dfthiluaydw");
        assert_eq!(entries[0].get_account(), "ad");
        assert_eq!(entries[1].get_label(), "zdrgzdrg");
        assert_eq!(entries[1].get_issuer(), "zdrgzdrg");
        assert_eq!(entries[1].get_account(), "zdrgzdrg");
    }

    #[test]
    fn ingest_csv_export_imports_totp_rows() {
        let entries = ingest_csv_export(PROTON_CSV_EXPORT.as_bytes()).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].get_label(), "Example 2FA");
        assert_eq!(entries[0].get_issuer(), "Example");
        assert_eq!(entries[0].get_account(), "alice@google.com");
        assert_eq!(entries[1].get_label(), "Example 2FA 2");
        assert_eq!(entries[1].get_issuer(), "Example");
        assert_eq!(entries[1].get_account(), "alice@google.com");
    }

    #[test]
    fn ingest_json_export_skips_login_items_without_totp_uri() {
        let entries = ingest_json_export(PROTON_JSON_EXPORT_WITH_MISSING_TOTP_URI.as_bytes()).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "Example 2FA");
        assert_eq!(entries[0].get_issuer(), "Example");
        assert_eq!(entries[0].get_account(), "alice@google.com");
    }

    #[test]
    fn ingest_json_export_accepts_bare_totp_secret() {
        let db = br#"{
            "vaults": {
                "vault-1": {
                    "items": [
                        {
                            "data": {
                                "metadata": { "name": "Example 2FA" },
                                "type": "login",
                                "content": {
                                    "totpUri": "ZBSWY3DPEHPK3PXP"
                                }
                            }
                        }
                    ]
                }
            }
        }"#;

        let entries = ingest_json_export(db).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "Example 2FA");
        assert_eq!(entries[0].get_account(), "Example 2FA");
        assert_eq!(entries[0].get_issuer(), "");
    }

    #[test]
    fn ingest_csv_export_accepts_bare_totp_secret() {
        let export = b"type,name,url,email,username,password,note,totp,createTime,modifyTime,vault\n\
login,Example 2FA,,,,,,ZBSWY3DPEHPK3PXP,1781999631,1781999631,Personal\n";

        let entries = ingest_csv_export(export).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "Example 2FA");
        assert_eq!(entries[0].get_account(), "Example 2FA");
        assert_eq!(entries[0].get_issuer(), "");
    }

    #[test]
    fn ingest_json_export_uses_fallback_name_for_bare_totp_secret_without_metadata_name() {
        let db = br#"{
            "vaults": {
                "vault-1": {
                    "items": [
                        {
                            "data": {
                                "metadata": { "name": null },
                                "type": "login",
                                "content": {
                                    "totpUri": "ZBSWY3DPEHPK3PXP"
                                }
                            }
                        }
                    ]
                }
            }
        }"#;

        let entries = ingest_json_export(db).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), tr::lookup_id(crate::TrId::MainImportNoName));
        assert_eq!(entries[0].get_account(), tr::lookup_id(crate::TrId::MainImportNoName));
    }

    #[test]
    fn ingest_json_export_is_deterministic_across_vault_keys() {
        let db = br#"{
            "vaults": {
                "vault-b": {
                    "items": [
                        {
                            "data": {
                                "metadata": { "name": "B" },
                                "type": "login",
                                "content": {
                                    "totpUri": "otpauth://totp/B:b?secret=JBSWY3DPEHPK3PXP&issuer=B"
                                }
                            }
                        }
                    ]
                },
                "vault-a": {
                    "items": [
                        {
                            "data": {
                                "metadata": { "name": "A" },
                                "type": "login",
                                "content": {
                                    "totpUri": "otpauth://totp/A:a?secret=JBSWY3DPEHPK3PXP&issuer=A"
                                }
                            }
                        }
                    ]
                }
            }
        }"#;

        let entries = ingest_json_export(db).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].get_label(), "A");
        assert_eq!(entries[1].get_label(), "B");
    }

    #[test]
    fn ingest_json_export_uses_uri_label_without_metadata_name() {
        let db = br#"{
            "vaults": {
                "vault-1": {
                    "items": [
                        {
                            "data": {
                                "metadata": {},
                                "type": "login",
                                "content": {
                                    "totpUri": "otpauth://totp/Broken:alice?issuer=Broken&secret=JBSWY3DPEHPK3PXP"
                                }
                            }
                        },
                        {
                            "data": {
                                "metadata": { "name": "Example 2FA" },
                                "type": "login",
                                "content": {
                                    "totpUri": "otpauth://totp/Example:alice%40google.com?issuer=Example&secret=ZBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30"
                                }
                            }
                        }
                    ]
                }
            }
        }"#;

        let entries = ingest_json_export(db).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].get_label(), "Broken");
        assert_eq!(entries[0].get_account(), "alice");
        assert_eq!(entries[1].get_label(), "Example 2FA");
    }

    #[test]
    fn ingest_authenticator_plain_export_rejects_malformed_entries() {
        let export = br#"{
            "version": 1,
            "entries": [
                {
                    "content": {
                        "uri": "otpauth://totp/Broken?issuer=Broken",
                        "entry_type": "Totp",
                        "name": "Broken"
                    }
                },
                {
                    "content": {
                        "uri": "otpauth://totp/Example:alice%40google.com?issuer=Example&secret=ZBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30",
                        "entry_type": "Totp",
                        "name": "Example 2FA"
                    }
                }
            ]
        }"#;

        let error = ingest_authenticator_plain_export(export).unwrap_err().to_string();
        assert!(error.contains("missing a secret"));
    }

    #[test]
    fn ingest_authenticator_plain_export_accepts_null_name() {
        let export = br#"{
            "version": 1,
            "entries": [
                {
                    "content": {
                        "uri": "otpauth://totp/Microsoft:example%40outlook.com?issuer=Microsoft&secret=ZBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30",
                        "entry_type": "Totp",
                        "name": null
                    }
                }
            ]
        }"#;

        let entries = ingest_authenticator_plain_export(export).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "Microsoft");
        assert_eq!(entries[0].get_issuer(), "Microsoft");
        assert_eq!(entries[0].get_account(), "example@outlook.com");
    }

    #[test]
    fn ingest_csv_export_uses_fallback_name_for_bare_totp_secret_without_name() {
        let export = b"type,name,url,email,username,password,note,totp,createTime,modifyTime,vault\n\
login,,,,,,,ZBSWY3DPEHPK3PXP,1781999631,1781999631,Personal\n";

        let entries = ingest_csv_export(export).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), tr::lookup_id(crate::TrId::MainImportNoName));
        assert_eq!(entries[0].get_account(), tr::lookup_id(crate::TrId::MainImportNoName));
    }

    #[test]
    fn ingest_authenticator_plain_export_rejects_non_totp_uri_scheme() {
        let export = br#"{
            "version": 1,
            "entries": [
                {
                    "content": {
                        "uri": "otpauth://hotp/Example:alice?issuer=Example&secret=ZBSWY3DPEHPK3PXP",
                        "entry_type": "Totp",
                        "name": "Example"
                    }
                }
            ]
        }"#;

        let error = ingest_authenticator_plain_export(export).unwrap_err().to_string();
        assert!(error.contains("otpauth://totp/"));
    }

    #[test]
    fn ingest_csv_export_rejects_malformed_rows() {
        let export = b"type,name,url,email,username,password,note,totp,createTime,modifyTime,vault\n\
login,Broken,,,,,,otpauth://totp/Broken?issuer=Broken,1781999631,1781999631,Personal\n\
login,Example 2FA,,,,,,otpauth://totp/Example:alice%40google.com?issuer=Example&secret=ZBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30,1781999631,1781999631,Personal\n";

        let error = ingest_csv_export(export).unwrap_err().to_string();
        assert!(error.contains("Invalid TOTP URL") || error.contains("missing a secret"));
    }

    #[test]
    fn ingest_json_export_uses_account_as_label_when_issuer_and_name_are_missing() {
        let db = br#"{
            "vaults": {
                "vault-1": {
                    "items": [
                        {
                            "data": {
                                "metadata": { "name": "" },
                                "type": "login",
                                "content": {
                                    "totpUri": "otpauth://totp/alice%40google.com?secret=ZBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30"
                                }
                            }
                        }
                    ]
                }
            }
        }"#;

        let entries = ingest_json_export(db).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "alice@google.com");
        assert_eq!(entries[0].get_account(), "alice@google.com");
        assert_eq!(entries[0].get_issuer(), "");
    }

    #[test]
    fn ingest_csv_export_uses_account_as_label_when_issuer_and_name_are_missing() {
        let export = b"type,name,url,email,username,password,note,totp,createTime,modifyTime,vault\n\
login,,,,,,,otpauth://totp/alice%40google.com?secret=ZBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30,1781999631,1781999631,Personal\n";

        let entries = ingest_csv_export(export).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "alice@google.com");
        assert_eq!(entries[0].get_account(), "alice@google.com");
        assert_eq!(entries[0].get_issuer(), "");
    }
}
