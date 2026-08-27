// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::{
        auth::{build_totp_url, make_totp_auth},
        import_crypto::{decrypt_gcm, GcmDecryptError},
        kdf::{self, Pbkdf2Sha256Request},
        tr, Auth, CryptoApi, TrId,
    },
    anyhow::{anyhow, Context},
    base64::{engine::general_purpose::STANDARD as BASE64, Engine},
    serde::{de::IgnoredAny, Deserialize},
    subtle::ConstantTimeEq,
    url::{form_urlencoded, Url},
};

const MIN_SCHEMA_VERSION: u32 = 1;
const MAX_SCHEMA_VERSION: u32 = 4;
const PBKDF2_ITERATIONS: u32 = 10_000;
const AES_KEY_LEN: usize = 32;
const GCM_NONCE_LEN: usize = 12;
const GCM_TAG_LEN: usize = 16;
const SALT_LEN: usize = 256;
// Padded Base64 uses 4 characters per 3 bytes; decode_salt enforces the exact length.
const MAX_ENCODED_SALT_LEN: usize = ((SALT_LEN + 2) / 3) * 4;
const REFERENCE: &[u8] = b"tRViSsLKzd86Hprh4ceC2OP7xazn4rrt4xhfEUbOjxLX8Rc3mkISXE0lWbmnWfggogbBJhtYgpK6fMl1D6mtsy92R3HkdGfwuXbzLebqVFJsR7IZ2w58t938iymwG4824igYy1wi6n2WDpO1Q1P69zwJGs2F5a1qP4MyIiDSD7NCV2OvidXQCBnDlGfmz0f1BQySRkkt4ryiJeCjD2o4QsveJ9uDBUn8ELyOrESv5R5DMDkD4iAF8TXU7KyoJujd";

#[derive(Debug, thiserror::Error)]
pub enum TwoFasError {
    #[error("2FAS import password did not decrypt the file")]
    PasswordMismatch,
    #[error(transparent)]
    Generic(#[from] anyhow::Error),
}

pub enum ParsedTwoFasExport {
    Plain,
    Encrypted(EncryptedExport),
}

#[derive(Clone)]
pub struct EncryptedExport {
    services: EncryptedData,
    reference: EncryptedData,
}

#[derive(Clone)]
struct EncryptedData {
    ciphertext: Vec<u8>,
    salt: [u8; SALT_LEN],
    nonce: [u8; GCM_NONCE_LEN],
    tag: [u8; GCM_TAG_LEN],
}

#[derive(Deserialize)]
struct ExportProbe {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    #[serde(rename = "appOrigin")]
    app_origin: String,
    #[serde(rename = "services")]
    _services: IgnoredAny,
    #[serde(default, rename = "servicesEncrypted")]
    services_encrypted: Option<String>,
    #[serde(default)]
    reference: Option<String>,
}

#[derive(Deserialize)]
struct PlainExport {
    services: Vec<Service>,
}

#[derive(Deserialize)]
struct Service {
    name: String,
    secret: String,
    otp: Otp,
}

#[derive(Deserialize)]
struct Otp {
    label: Option<String>,
    account: Option<String>,
    issuer: Option<String>,
    digits: Option<u32>,
    period: Option<u64>,
    algorithm: Option<String>,
    link: Option<String>,
    #[serde(rename = "tokenType")]
    token_type: Option<String>,
}

pub fn parse_export(bytes: &[u8]) -> Result<ParsedTwoFasExport, TwoFasError> {
    let probe: ExportProbe = crate::from_import_json(bytes, "Invalid 2FAS export JSON")?;
    validate_metadata(probe.schema_version, &probe.app_origin)?;

    match (probe.services_encrypted, probe.reference) {
        (None, None) => Ok(ParsedTwoFasExport::Plain),
        (Some(services), Some(reference)) => Ok(ParsedTwoFasExport::Encrypted(EncryptedExport {
            services: parse_encrypted_data(&services)?,
            reference: parse_encrypted_data(&reference)?,
        })),
        _ => Err(anyhow!("2FAS encrypted export is missing encrypted data").into()),
    }
}

pub async fn decrypt_export(
    crypto: &CryptoApi,
    export: &EncryptedExport,
    password: &str,
) -> Result<Vec<u8>, TwoFasError> {
    let reference = decrypt_data(crypto, &export.reference, password).await?;
    if !bool::from(reference.as_slice().ct_eq(REFERENCE)) {
        return Err(TwoFasError::PasswordMismatch);
    }

    decrypt_data(crypto, &export.services, password).await
}

pub fn ingest_plaintext_export(bytes: &[u8]) -> Result<Vec<Auth>, TwoFasError> {
    let export: PlainExport = crate::from_import_json(bytes, "Invalid 2FAS services JSON")?;
    ingest_services(export.services)
}

pub fn ingest_decrypted_services(bytes: &[u8]) -> Result<Vec<Auth>, TwoFasError> {
    let services = crate::from_import_json(bytes, "Invalid decrypted 2FAS services JSON")?;
    ingest_services(services)
}

fn ingest_services(services: Vec<Service>) -> Result<Vec<Auth>, TwoFasError> {
    let mut entries = Vec::new();
    for service in services {
        if service
            .otp
            .token_type
            .as_deref()
            .is_some_and(|token_type| !token_type.eq_ignore_ascii_case("totp"))
        {
            continue;
        }

        entries.push(parse_service(&service)?);
    }

    if entries.is_empty() {
        return Err(anyhow!("2FAS export contains no importable TOTP entries").into());
    }

    Ok(entries)
}

fn validate_metadata(schema_version: u32, app_origin: &str) -> anyhow::Result<()> {
    if !(MIN_SCHEMA_VERSION..=MAX_SCHEMA_VERSION).contains(&schema_version) {
        return Err(anyhow!("Unsupported 2FAS schema version {schema_version}"));
    }
    if app_origin != "android" && app_origin != "ios" {
        return Err(anyhow!("Unsupported 2FAS app origin"));
    }
    Ok(())
}

fn parse_encrypted_data(value: &str) -> anyhow::Result<EncryptedData> {
    let mut parts = value.split(':');
    let encoded_data = parts.next().context("2FAS encrypted data is missing ciphertext")?;
    let encoded_salt = parts.next().context("2FAS encrypted data is missing salt")?;
    let encoded_nonce = parts.next().context("2FAS encrypted data is missing nonce")?;
    if parts.next().is_some() {
        return Err(anyhow!("2FAS encrypted data has too many fields"));
    }

    let mut ciphertext = BASE64.decode(encoded_data).context("Invalid 2FAS ciphertext encoding")?;
    if ciphertext.len() <= GCM_TAG_LEN {
        return Err(anyhow!("2FAS encrypted data is too short"));
    }
    let tag = ciphertext
        .split_off(ciphertext.len() - GCM_TAG_LEN)
        .try_into()
        .map_err(|_| anyhow!("Invalid 2FAS authentication tag length"))?;

    Ok(EncryptedData {
        ciphertext,
        salt: decode_salt(encoded_salt)?,
        nonce: decode_fixed_base64(encoded_nonce, "nonce")?,
        tag,
    })
}

fn decode_fixed_base64<const N: usize>(encoded: &str, field: &str) -> anyhow::Result<[u8; N]> {
    let mut decoded = [0; N];
    let decoded_len = BASE64
        .decode_slice(encoded, &mut decoded)
        .with_context(|| format!("Invalid 2FAS {field} encoding"))?;
    if decoded_len != N {
        return Err(anyhow!("Invalid 2FAS {field} length"));
    }

    Ok(decoded)
}

fn decode_salt(encoded: &str) -> anyhow::Result<[u8; SALT_LEN]> {
    if encoded.len() > MAX_ENCODED_SALT_LEN {
        return Err(anyhow!("Invalid 2FAS salt length"));
    }

    decode_fixed_base64(encoded, "salt")
}

fn parse_service(service: &Service) -> Result<Auth, TwoFasError> {
    let fallback = tr::lookup_id(TrId::MainImportNoName).to_string();
    let url = match non_empty(service.otp.link.as_deref()) {
        Some(link) => replace_link_secret(link, &service.secret)?,
        None => {
            let account = non_empty(service.otp.account.as_deref())
                .or_else(|| non_empty(service.otp.label.as_deref()))
                .or_else(|| non_empty(Some(service.name.as_str())))
                .unwrap_or(&fallback);
            let issuer = non_empty(service.otp.issuer.as_deref());
            let algorithm = non_empty(service.otp.algorithm.as_deref()).unwrap_or("SHA1");
            let digits = service.otp.digits.unwrap_or(6);
            let period = service.otp.period.unwrap_or(30);
            build_totp_url(&service.secret, account, issuer, algorithm, digits, period)?
        }
    };

    make_totp_auth(&url, non_empty(Some(service.name.as_str()))).map_err(TwoFasError::from)
}

fn replace_link_secret(link: &str, secret: &str) -> anyhow::Result<String> {
    let mut url = Url::parse(link).context("Invalid 2FAS OTP link")?;
    let mut query = form_urlencoded::Serializer::new(String::new());
    let mut found_secret = false;

    for (name, value) in url.query_pairs() {
        if name == "secret" {
            if !found_secret {
                query.append_pair("secret", secret);
                found_secret = true;
            }
        } else {
            query.append_pair(&name, &value);
        }
    }
    if !found_secret {
        query.append_pair("secret", secret);
    }

    url.set_query(Some(&query.finish()));
    Ok(url.to_string())
}

fn non_empty(value: Option<&str>) -> Option<&str> { value.filter(|value| !value.is_empty()) }

async fn decrypt_data(
    crypto: &CryptoApi,
    encrypted: &EncryptedData,
    password: &str,
) -> Result<Vec<u8>, TwoFasError> {
    let key = kdf::derive_pbkdf2_sha256(Pbkdf2Sha256Request {
        password: password.as_bytes().to_vec(),
        salt: encrypted.salt.to_vec(),
        iterations: PBKDF2_ITERATIONS,
        out_len: AES_KEY_LEN,
    })
    .await
    .map_err(anyhow::Error::new)?;

    let plaintext =
        decrypt_gcm(crypto, key.as_slice(), encrypted.nonce, encrypted.tag, &encrypted.ciphertext, None)
            .map_err(|error| match error {
                GcmDecryptError::AuthenticationFailed => TwoFasError::PasswordMismatch,
                GcmDecryptError::Operation(error) => error.into(),
            })?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use ordered_table::SortableCard;

    use super::*;
    use crate::auth::get_timestamp_in_seconds;

    #[test]
    fn parses_plain_export_and_skips_hotp() {
        let export = br#"{
            "schemaVersion": 4,
            "appOrigin": "android",
            "services": [
                {
                    "name": "Example",
                    "secret": "JBSWY3DPEHPK3PXP",
                    "otp": {
                        "link": "otpauth://totp/Example:alice%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=Example&algorithm=SHA1&digits=6&period=30",
                        "account": "wrong-account",
                        "issuer": "Example",
                        "digits": 6,
                        "period": 30,
                        "algorithm": "SHA1",
                        "tokenType": "TOTP"
                    }
                },
                {
                    "name": "Counter",
                    "secret": "JBSWY3DPEHPK3PXP",
                    "otp": {
                        "account": "counter@example.com",
                        "counter": 1,
                        "tokenType": "HOTP"
                    }
                }
            ],
            "servicesEncrypted": null,
            "reference": null
        }"#;

        let ParsedTwoFasExport::Plain = parse_export(export).unwrap() else {
            panic!("expected plain export");
        };
        let entries = ingest_plaintext_export(export).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "Example");
        assert_eq!(entries[0].get_account(), "alice@example.com");
        assert_eq!(entries[0].get_issuer(), "Example");
    }

    #[test]
    fn replaces_link_secret_with_exported_secret() {
        let export = br#"{
            "schemaVersion": 4,
            "appOrigin": "android",
            "services": [
                {
                    "name": "Example",
                    "secret": "JBSWY3DPEHPK3PXP",
                    "otp": {
                        "link": "otpauth://totp/Example:alice%40example.com?secret=%5Bhidden%5D&issuer=Example&algorithm=SHA1&digits=6&period=30",
                        "tokenType": "TOTP"
                    }
                }
            ]
        }"#;

        let entries = ingest_plaintext_export(export).unwrap();
        let expected = Auth::new(
            "otpauth://totp/Example:alice%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=Example&algorithm=SHA1&digits=6&period=30"
                .to_string(),
            get_timestamp_in_seconds(),
        )
        .unwrap();
        assert_eq!(entries, vec![expected]);
    }

    #[test]
    fn parses_service_without_otp_link() {
        let export = br#"{
            "schemaVersion": 4,
            "appOrigin": "android",
            "services": [
                {
                    "name": "Example",
                    "secret": "JBSWY3DPEHPK3PXP",
                    "otp": {
                        "account": "alice@example.com",
                        "issuer": "Example",
                        "digits": 6,
                        "period": 30,
                        "algorithm": "SHA1",
                        "tokenType": "TOTP"
                    }
                }
            ]
        }"#;

        let entries = ingest_plaintext_export(export).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_label(), "Example");
        assert_eq!(entries[0].get_account(), "alice@example.com");
        assert_eq!(entries[0].get_issuer(), "Example");
    }

    #[test]
    fn parses_encrypted_export_shape() {
        let payload = BASE64.encode([0u8; GCM_TAG_LEN + 1]);
        let salt = BASE64.encode([1u8; SALT_LEN]);
        let nonce = BASE64.encode([2u8; GCM_NONCE_LEN]);
        let encrypted = format!("{payload}:{salt}:{nonce}");
        let export = format!(
            r#"{{
                "schemaVersion": 4,
                "appOrigin": "ios",
                "services": [],
                "servicesEncrypted": "{encrypted}",
                "reference": "{encrypted}"
            }}"#
        );

        let ParsedTwoFasExport::Encrypted(export) = parse_export(export.as_bytes()).unwrap() else {
            panic!("expected encrypted export");
        };
        assert_eq!(export.services.ciphertext, vec![0]);
        assert_eq!(export.services.salt, [1; SALT_LEN]);
        assert_eq!(export.services.nonce, [2; GCM_NONCE_LEN]);
        assert_eq!(export.services.tag, [0; GCM_TAG_LEN]);
        assert!(ingest_decrypted_services(b"[]").is_err());
    }

    #[test]
    fn rejects_plain_export_with_only_unsupported_services() {
        let export = br#"{
            "schemaVersion": 4,
            "appOrigin": "android",
            "services": [
                {
                    "name": "Counter",
                    "secret": "JBSWY3DPEHPK3PXP",
                    "otp": {
                        "account": "counter@example.com",
                        "counter": 1,
                        "tokenType": "HOTP"
                    }
                }
            ]
        }"#;

        assert!(ingest_plaintext_export(export).is_err());
    }

    #[test]
    fn rejects_invalid_encrypted_salt_lengths() {
        let payload = BASE64.encode([0u8; GCM_TAG_LEN + 1]);
        let nonce = BASE64.encode([2u8; GCM_NONCE_LEN]);

        for salt_len in [SALT_LEN - 1, SALT_LEN + 1] {
            let salt = BASE64.encode(vec![1u8; salt_len]);
            let encrypted = format!("{payload}:{salt}:{nonce}");

            assert!(parse_encrypted_data(&encrypted).is_err());
        }
    }

    #[test]
    fn rejects_oversized_encoded_salt_before_decoding() {
        let payload = BASE64.encode([0u8; GCM_TAG_LEN + 1]);
        let nonce = BASE64.encode([2u8; GCM_NONCE_LEN]);
        let oversized_salt = "A".repeat(MAX_ENCODED_SALT_LEN + 1);
        let encrypted = format!("{payload}:{oversized_salt}:{nonce}");

        assert!(parse_encrypted_data(&encrypted).is_err());
    }

    #[test]
    fn accepts_supported_schema_versions() {
        for schema_version in MIN_SCHEMA_VERSION..=MAX_SCHEMA_VERSION {
            let export = format!(
                r#"{{
                    "schemaVersion": {schema_version},
                    "appOrigin": "android",
                    "services": []
                }}"#
            );
            assert!(matches!(parse_export(export.as_bytes()).unwrap(), ParsedTwoFasExport::Plain));
        }
    }

    #[test]
    fn rejects_incomplete_encrypted_export() {
        let export = br#"{
            "schemaVersion": 4,
            "appOrigin": "android",
            "services": [],
            "servicesEncrypted": "AA==:AA==:AA=="
        }"#;

        assert!(parse_export(export).is_err());
    }
}
