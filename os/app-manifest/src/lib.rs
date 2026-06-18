// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

#[cfg(not(keyos))]
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod schema;

/// Length of an `app_id` hex string without the `0x` prefix.
pub const APP_ID_HEX_LEN: usize = 32;
pub const APP_ID_BYTE_LEN: usize = 16;

#[derive(Debug, Clone, Error, PartialEq)]
pub enum AppIdParseError {
    #[error("AppId must start with 0x")]
    MissingPrefix,
    #[error("Invalid AppId hex length: {actual}, expected {expected}")]
    InvalidLength { actual: usize, expected: usize },
    #[error("Invalid AppId hex: {0}")]
    InvalidHex(hex::FromHexError),
}

/// Parse a `"0x"`-prefixed, 32-character hex `app_id` into its 16 bytes.
pub fn parse_app_id_bytes(app_id: &str) -> Result<[u8; APP_ID_BYTE_LEN], AppIdParseError> {
    let hex_app_id = app_id.strip_prefix("0x").ok_or(AppIdParseError::MissingPrefix)?;

    if hex_app_id.len() != APP_ID_HEX_LEN {
        return Err(AppIdParseError::InvalidLength { actual: hex_app_id.len(), expected: APP_ID_HEX_LEN });
    }

    let mut app_id_bytes = [0u8; APP_ID_BYTE_LEN];
    hex::decode_to_slice(hex_app_id, &mut app_id_bytes).map_err(AppIdParseError::InvalidHex)?;
    Ok(app_id_bytes)
}

/// Locale format, e.g. "en", "fr", etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct Locale(pub String);

impl From<String> for Locale {
    fn from(value: String) -> Self { Locale(value) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: usize,
    pub r#type: MessageType,
    pub description: Option<String>,
    pub cfg: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageType {
    Archive,
    BlockingArchive,
    ArchiveEvent,
    Scalar,
    BlockingScalar,
    ScalarEvent,
    LendMut,
    DeferredLendMut,
    Move,
}

/// Current manifest schema. Update this alias when introducing a new breaking version.
pub type Manifest = schema::ManifestV0;
pub type QrPriority = schema::QrPriorityV0;
pub type QrMatchRule = schema::QrMatchRuleV0;
pub type QrMatchSubRule = schema::QrMatchSubRuleV0;

/// Current API manifest schema. Update this alias when introducing a new breaking version.
pub type ApiManifest = schema::ApiManifestV0;

// Methods on the current manifest versions. These live here — not in the version files —
// so that frozen version files never need editing. The impl blocks use the type aliases
// directly, so no version-specific names appear here; only the aliases above need updating.

impl Manifest {
    #[cfg(not(keyos))]
    pub fn load(crate_dir: &Path, templates_dir: &Path) -> Self {
        Self::load_with_tracking(crate_dir, templates_dir, |_| {})
    }

    #[cfg(not(keyos))]
    pub fn load_with_tracking(crate_dir: &Path, templates_dir: &Path, mut track: impl FnMut(&Path)) -> Self {
        load::load_server_manifest(crate_dir, templates_dir, &mut track)
    }

    pub fn app_name_en(&self) -> String {
        self.app_name.get(&Locale("en".into())).cloned().unwrap_or("N/A".to_string())
    }
}

impl ApiManifest {
    #[cfg(not(keyos))]
    pub fn load_with_tracking(crate_dir: &Path, mut track: impl FnMut(&Path)) -> Self {
        load::load_api_manifest(crate_dir, &mut track)
    }
}

/// Parse a manifest from JSON bytes, migrating to the current schema version as needed.
/// Version dispatch and migration chaining live in `schema::migrate_json`.
pub fn try_from_bytes(bytes: &[u8]) -> Result<Manifest, serde_json::Error> { schema::migrate_json(bytes) }

/// Manifest describing one service the hosted-mode kernel should spawn.
/// Written by xtask, read by the kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedService {
    pub path: String,
    #[serde(with = "app_id_hex")]
    pub app_id: [u8; APP_ID_BYTE_LEN],
    pub syscalls: u64,
    /// If this service exits, the hosted kernel shuts down with it.
    #[serde(default)]
    pub system: bool,
}

/// Serde `with` codec mapping `app_id` between its `"0x"`-prefixed hex wire form and
/// `[u8; APP_ID_BYTE_LEN]`, so a malformed id is rejected at deserialize time.
pub(crate) mod app_id_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::{parse_app_id_bytes, APP_ID_BYTE_LEN};

    pub fn serialize<S: Serializer>(bytes: &[u8; APP_ID_BYTE_LEN], s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&format_args!("0x{}", hex::encode(bytes)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; APP_ID_BYTE_LEN], D::Error> {
        let s = String::deserialize(d)?;
        parse_app_id_bytes(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_APP_ID: &str = "0xbf5cdfbfda7e85b5253ff268d32ea957";

    fn v0_json(extra: &str) -> String {
        format!(r#"{{"manifestVersion":"0","appName":{{"en":"Test"}},"appId":"{}"{}}}"#, VALID_APP_ID, extra)
    }

    #[test]
    fn try_from_bytes_v0_parses_successfully() {
        let manifest = try_from_bytes(v0_json("").as_bytes()).unwrap();
        assert_eq!(manifest.app_name_en(), "Test");
    }

    #[test]
    fn try_from_bytes_missing_version_defaults_to_v0() {
        let json = format!(r#"{{"appName":{{"en":"Test"}},"appId":"{}"}}"#, VALID_APP_ID);
        let manifest = try_from_bytes(json.as_bytes()).unwrap();
        assert_eq!(manifest.app_name_en(), "Test");
    }

    #[test]
    fn try_from_bytes_unknown_version_fails() {
        let json =
            format!(r#"{{"manifestVersion":"99","appName":{{"en":"Test"}},"appId":"{}"}}"#, VALID_APP_ID);
        assert!(try_from_bytes(json.as_bytes()).is_err());
    }

    #[test]
    fn qr_match_rule_priority_defaults_to_three() {
        let manifest = try_from_bytes(
            v0_json(r#","qrMatchRules":[{"id":"rule","subRules":{"qr":{"QR":{"min_len":1}}}}]"#).as_bytes(),
        )
        .unwrap();
        assert_eq!(manifest.qr_match_rules[0].priority, QrPriority::default());
    }

    #[test]
    fn qr_match_rule_priority_rejects_out_of_range_values() {
        let err = try_from_bytes(
            v0_json(r#","qrMatchRules":[{"id":"rule","priority":0,"subRules":{"qr":{"QR":{"min_len":1}}}}]"#)
                .as_bytes(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("QR priority must be between 1 and 5"));
    }

    #[test]
    fn try_from_bytes_invalid_app_id_fails() {
        let json = format!(r#"{{"appName":{{"en":"Test"}},"appId":"{}"}}"#, "0xnope");
        assert!(try_from_bytes(json.as_bytes()).is_err());
    }

    #[test]
    fn app_id_parser_rejects_missing_prefix() {
        assert_eq!(
            parse_app_id_bytes("00000000000000000000000000000001").unwrap_err(),
            AppIdParseError::MissingPrefix
        );
    }

    #[test]
    fn app_id_parser_rejects_invalid_hex() {
        assert!(matches!(
            parse_app_id_bytes("0x0000000000000000000000000000000g"),
            Err(AppIdParseError::InvalidHex(_))
        ));
    }

    #[test]
    fn app_id_parser_rejects_short_length() {
        assert_eq!(
            parse_app_id_bytes("0x01").unwrap_err(),
            AppIdParseError::InvalidLength { actual: 2, expected: APP_ID_HEX_LEN }
        );
    }

    #[test]
    fn app_id_parser_rejects_long_length() {
        assert_eq!(
            parse_app_id_bytes("0x0000000000000000000000000000000100").unwrap_err(),
            AppIdParseError::InvalidLength { actual: 34, expected: APP_ID_HEX_LEN }
        );
    }

    #[test]
    fn app_id_parser_accepts_valid_app_id() {
        assert_eq!(
            parse_app_id_bytes("0x00000000000000000000000000000001").unwrap(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
    }
}

#[cfg(not(keyos))]
mod load {
    use super::*;

    fn read_manifest_content(crate_dir: &Path, track: &mut impl FnMut(&Path)) -> String {
        let path = crate_dir.join("manifest.toml");
        track(&path);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to read manifest file at {:?}: {:?}", path, e))
    }

    /// Load an API manifest
    pub fn load_api_manifest(crate_dir: &Path, track: &mut impl FnMut(&Path)) -> ApiManifest {
        let content = read_manifest_content(crate_dir, track);
        let mut manifest = schema::migrate_api_toml(&content, crate_dir);

        if let Some(extends) = &manifest.extends.clone() {
            let extends = crate_dir.join(extends);
            let extends = std::fs::canonicalize(&extends)
                .unwrap_or_else(|e| panic!("Failed to resolve extends path {:?}: {:?}", extends, e));

            let extends_manifest = load_api_manifest(&extends, track);

            for (name, messages) in extends_manifest.servers {
                let entry = manifest.servers.entry(name).or_default();
                for (msg_name, msg) in messages {
                    entry.entry(msg_name).or_insert(msg);
                }
            }
        }

        manifest
    }

    /// Load a full server manifest
    pub fn load_server_manifest(
        crate_dir: &Path,
        templates_dir: &Path,
        track: &mut impl FnMut(&Path),
    ) -> Manifest {
        let content = read_manifest_content(crate_dir, track);
        let mut manifest = schema::migrate_server_toml(&content, crate_dir);

        let api_manifest = load_api_manifest(crate_dir, track);
        manifest.servers = api_manifest.servers;

        expand_permission_templates(&mut manifest, templates_dir);

        manifest
    }

    /// Expand permission templates into actual permissions
    fn expand_permission_templates(manifest: &mut Manifest, templates_dir: &Path) {
        let path = templates_dir.join("permission_templates.toml");
        let template_file = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to read permission template file at {:?}: {:?}", path, e));
        let templates: BTreeMap<String, BTreeMap<String, Vec<String>>> = toml::from_str(&template_file)
            .unwrap_or_else(|e| panic!("Failed to parse permission template file at {:?}: {:?}", path, e));

        if let Some(used_templates) = manifest.permissions.get_mut("template") {
            let mut remaining = BTreeSet::new();
            for template_name in used_templates.clone().iter() {
                let Some(additional_permissions) = templates.get(template_name) else {
                    remaining.insert(template_name.clone());
                    continue;
                };
                for (server_name, messages) in additional_permissions {
                    manifest
                        .permissions
                        .entry(server_name.clone())
                        .or_default()
                        .extend(messages.iter().cloned());
                }
            }
            if remaining.is_empty() {
                manifest.permissions.remove("template");
            } else {
                manifest.permissions.insert("template".into(), remaining);
            }
        }
    }
}
