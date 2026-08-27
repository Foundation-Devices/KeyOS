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
pub const FILE_HASH_BYTE_LEN: usize = 32;

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
    /// Permission subgroup this message belongs to, as `"<group>.<subgroup>"` (e.g.
    /// `"peripherals.camera-use"`); groups the message in the permission UI. Use a
    /// group that exists in the app manager's `GROUP_LABELS`, or the UI falls back
    /// to showing the raw key. Its presence also drives the
    /// [`Message::required_signature`] default, so adding a group opens the message
    /// to third-party apps.
    #[serde(rename = "permissionGroup", default, skip_serializing_if = "Option::is_none")]
    pub permission_group: Option<String>,
    /// The signature a sender must carry to hold this permission. Absent defaults from
    /// [`Message::permission_group`]: a grouped message is grantable to third-party apps, so it
    /// defaults to [`RequiredSignature::ThirdParty`]; an ungrouped message defaults to
    /// [`RequiredSignature::Foundation`]. Set explicitly to override.
    #[serde(rename = "requiredSignature", default, skip_serializing_if = "Option::is_none")]
    pub required_signature: Option<RequiredSignature>,
    /// How the permission is granted to a sideloaded app. Defaults to
    /// [`ApprovalBehavior::NotUserGrantable`] when a message declares no `approval`.
    #[serde(default, skip_serializing_if = "ApprovalBehavior::is_not_user_grantable")]
    pub approval: ApprovalBehavior,
}

impl Message {
    /// The signature a sender must carry to hold this permission, applying the
    /// group-derived default when the manifest sets none.
    pub fn required_signature(&self) -> RequiredSignature {
        self.required_signature.unwrap_or({
            // A message carrying a permission group is grantable to third-party apps; an
            // ungrouped message is Foundation-only (built-in services and Foundation apps).
            if self.permission_group.is_some() {
                RequiredSignature::ThirdParty
            } else {
                RequiredSignature::Foundation
            }
        })
    }
}

/// The signature a sender must carry to be granted a message permission. A manifest that
/// omits it gets a default derived from `permissionGroup`; see
/// [`Message::required_signature`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RequiredSignature {
    /// Any validly signed app, including sideloaded third-party apps.
    ThirdParty,
    /// Only Foundation-signed processes (built-in services and Foundation apps).
    Foundation,
}

/// How a message permission is granted to a sideloaded app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalBehavior {
    /// Granted without a prompt. Not the same as open to anyone: the sender still has to
    /// declare the message in its own manifest and pass the signature gate.
    AutoAllow,
    /// The user is prompted once per permission subgroup, not per message; the answer persists.
    GrantOnFirstUse,
    /// Never grantable through the permission UI. The default when a message declares no
    /// `approval`.
    #[default]
    NotUserGrantable,
}

impl ApprovalBehavior {
    pub fn is_approval_based(self) -> bool { matches!(self, Self::GrantOnFirstUse) }

    /// Whether this is the default (used to omit it from serialized manifests).
    pub fn is_not_user_grantable(&self) -> bool { matches!(self, Self::NotUserGrantable) }
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

/// Serde `with` codec mapping `fileHashes` between its bare hex wire form and
/// `[u8; FILE_HASH_BYTE_LEN]`, so a value that is not a sha256 digest is rejected at
/// deserialize time rather than travelling as an arbitrary string.
pub(crate) mod file_hashes_hex {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serializer};

    use super::FILE_HASH_BYTE_LEN;

    type Hashes = BTreeMap<String, [u8; FILE_HASH_BYTE_LEN]>;

    pub fn serialize<S: Serializer>(hashes: &Hashes, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(hashes.iter().map(|(path, hash)| (path, hex::encode(hash))))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hashes, D::Error> {
        BTreeMap::<String, String>::deserialize(d)?
            .into_iter()
            .map(|(path, hex_hash)| {
                let mut hash = [0u8; FILE_HASH_BYTE_LEN];
                hex::decode_to_slice(&hex_hash, &mut hash)
                    .map_err(|e| serde::de::Error::custom(format!("{path}: {e}")))?;
                Ok((path, hash))
            })
            .collect()
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
    fn minimum_keyos_version_round_trips_as_camel_case() {
        let manifest = try_from_bytes(v0_json(r#","minKeyosVersion":"1.4.0-beta1""#).as_bytes()).unwrap();
        assert_eq!(manifest.min_keyos_version, Some(semver::Version::parse("1.4.0-beta1").unwrap()));
        assert!(serde_json::to_string(&manifest).unwrap().contains(r#""minKeyosVersion":"1.4.0-beta1""#));
    }

    #[test]
    fn app_version_stays_opaque_while_minimum_keyos_is_semver() {
        let manifest =
            try_from_bytes(v0_json(r#","version":"2.1.0","minKeyosVersion":"1.2.3-beta1""#).as_bytes())
                .unwrap();

        assert_eq!(manifest.version.as_deref(), Some("2.1.0"));
        assert_eq!(manifest.min_keyos_version, Some(semver::Version::parse("1.2.3-beta1").unwrap()));
    }

    #[test]
    fn app_version_accepts_legacy_formats_but_minimum_keyos_requires_semver() {
        let manifest = try_from_bytes(v0_json(r#","version":"2026.08-beta""#).as_bytes()).unwrap();
        assert_eq!(manifest.version.as_deref(), Some("2026.08-beta"));
        assert!(try_from_bytes(v0_json(r#","minKeyosVersion":"banana""#).as_bytes()).is_err());
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

    /// Every signed manifest in the field carries bare hex, and the signature covers those exact
    /// bytes, so decoding to an array must not move the wire form.
    #[test]
    fn file_hashes_keep_their_bare_hex_wire_form() {
        const HASH: &str = "14af488a6c10ee9b5f628bfb1c1f01b27965f583aafdd75bfaeefd39fbbcb221";

        let manifest =
            try_from_bytes(v0_json(&format!(r#","fileHashes":{{"app.elf":"{HASH}"}}"#)).as_bytes()).unwrap();

        assert_eq!(manifest.file_hashes["app.elf"], hex::decode(HASH).unwrap()[..]);
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains(&format!(r#""fileHashes":{{"app.elf":"{HASH}"}}"#)), "{json}");
    }

    #[test]
    fn file_hashes_reject_anything_that_is_not_a_sha256_digest() {
        for value in ["", "not hex", "14af488a", &"ab".repeat(33)] {
            let json = v0_json(&format!(r#","fileHashes":{{"app.elf":"{value}"}}"#));
            assert!(try_from_bytes(json.as_bytes()).is_err(), "accepted {value:?}");
        }
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
    fn qr_match_rules_ignore_unknown_fields() {
        for field in ["regex-pattern", "regexPattern"] {
            let manifest = try_from_bytes(
                v0_json(&format!(
                    r#","qrMatchRules":[{{"id":"rule","subRules":{{"qr":{{"QR":{{"{field}":"^test$"}}}}}}}}]"#
                ))
                .as_bytes(),
            )
            .unwrap();
            assert!(matches!(
                manifest.qr_match_rules[0].sub_rules["qr"],
                QrMatchSubRule::QR { regex_pattern: None, .. }
            ));
        }
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
