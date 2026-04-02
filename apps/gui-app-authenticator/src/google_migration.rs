// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use prost::Message;
use regex::Regex;

use crate::{get_timestamp_in_seconds, Auth, AuthValidationError};

#[path = "../proto/google_auth_migration.rs"]
mod proto;

use proto::migration_payload::{Algorithm, OtpType};

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("Invalid migration URI format")]
    InvalidUri,
    #[error("Failed to decode base64 data: {0}")]
    Base64DecodeError(#[from] base64::DecodeError),
    #[error("Protobuf decode error: {0}")]
    ProtobufDecodeError(String),
    #[error("All accounts were HOTP (unsupported)")]
    AllHotpSkipped,
    #[error("Auth validation error: {0}")]
    AuthValidationError(#[from] AuthValidationError),
}

#[derive(Debug, Clone)]
pub struct MigrationEntry {
    pub otpauth_url: String,
    pub label: String,
    pub account: String,
    pub issuer: String,
}

pub fn is_migration_uri(uri: &str) -> bool { uri.starts_with("otpauth-migration://offline") }

pub fn parse_migration_uri(uri: &str) -> Result<Vec<Auth>, MigrationError> {
    let re = Regex::new(r"^otpauth-migration://offline\?(.*)$").unwrap();
    let caps = re.captures(uri).ok_or(MigrationError::InvalidUri)?;

    let query_string = &caps[1];
    let data_param = extract_query_param(query_string, "data").ok_or(MigrationError::InvalidUri)?;

    let decoded_data = urlencoding::decode(&data_param)
        .map_err(|e| MigrationError::ProtobufDecodeError(e.to_string()))?
        .into_owned();
    let decoded = BASE64.decode(decoded_data)?;

    let payload = proto::MigrationPayload::decode(decoded.as_slice())
        .map_err(|e| MigrationError::ProtobufDecodeError(e.to_string()))?;

    let mut entries = Vec::new();

    for otp in &payload.otp_parameters {
        // Skip HOTP entries (type 1), only import TOTP (type 2)
        if otp.r#type != OtpType::Totp as i32 {
            log::info!("Skipping HOTP entry: {}", otp.name);
            continue;
        }

        let algo_str = match otp.algorithm {
            x if x == Algorithm::Sha1 as i32 => "SHA1",
            x if x == Algorithm::Sha256 as i32 => "SHA256",
            x if x == Algorithm::Sha512 as i32 => "SHA512",
            _ => "SHA1",
        };

        let secret = base32::encode(base32::Alphabet::RFC4648 { padding: false }, &otp.secret);

        let label = if !otp.issuer.is_empty() { otp.issuer.clone() } else { "No Label".to_string() };

        let url = format!(
            // 6 digits fow now are hardcoded since otp.digits contains nonsense data
            // Example: it returns 1 for code with 6 digits
            "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm={}&digits=6&period=30",
            urlencoding::encode(&label),
            urlencoding::encode(&otp.name),
            urlencoding::encode(&secret),
            urlencoding::encode(&label),
            urlencoding::encode(algo_str),
        );

        let auth = Auth::new(url, get_timestamp_in_seconds())?;
        entries.push(auth);
    }

    if entries.is_empty() {
        return Err(MigrationError::AllHotpSkipped);
    }

    Ok(entries)
}

fn extract_query_param(query: &str, key: &str) -> Option<String> {
    let re = Regex::new(&format!(r"{}=([^&]*)", regex::escape(key))).ok()?;
    let caps = re.captures(query)?;
    Some(caps[1].to_string())
}
