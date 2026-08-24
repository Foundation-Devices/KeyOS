// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Manifest conversion from SDK app config to the shared KeyOS manifest schema.

use std::collections::{BTreeMap, BTreeSet};

use app_manifest::{Locale, Manifest};

use crate::config::{AppConfig, PermissionEntries};

pub type AppManifest = Manifest;
pub use app_manifest::FILE_HASH_BYTE_LEN;

pub fn app_manifest_from_config(
    config: &AppConfig,
    app_version: &semver::Version,
    permissions: PermissionEntries,
) -> AppManifest {
    AppManifest {
        app_name: config
            .manifest_app_names()
            .into_iter()
            .map(|(locale, name)| (Locale(locale), name))
            .collect(),
        app_id: config.app_id.as_bytes().try_into().expect("AppConfig validation enforces 16-byte app IDs"),
        publisher: config.publisher.name_value().map(ToOwned::to_owned),
        description: (!config.description.trim().is_empty()).then(|| config.description.clone()),
        version: Some(app_version.clone()),
        min_keyos_version: Some(config.min_keyos_version.clone()),
        servers: BTreeMap::new(),
        fixed_sids: BTreeMap::new(),
        permissions: permissions_to_sets(permissions),
        memory: Vec::new(),
        syscall: Vec::new(),
        qr_match_rules: config.qr_match_rules.iter().cloned().map(Into::into).collect(),
        file_hashes: BTreeMap::new(),
    }
}

fn permissions_to_sets(permissions: PermissionEntries) -> BTreeMap<String, BTreeSet<String>> {
    permissions.into_iter().map(|(server, messages)| (server, messages.into_iter().collect())).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use app_manifest::Locale;

    use super::app_manifest_from_config;
    use crate::config::{
        AppConfig, AppId, PermissionEntries, PermissionsConfig, PublisherConfig, QrMatchRuleConfig,
        QrMatchSubRuleConfig,
    };

    fn demo_config(app_id: &str) -> AppConfig {
        AppConfig {
            app_name: "demo-app".to_string(),
            friendly_app_name: "Demo Friendly".to_string(),
            launcher_app_name: None,
            description: "Demo".to_string(),
            publisher: PublisherConfig { name: "Demo Corp".to_string(), ..Default::default() },
            icon: PathBuf::from("resources/icon.svg"),
            theme: None,
            app_id: AppId::from_hex(app_id).unwrap(),
            permissions: PermissionsConfig::default(),
            version: Some(semver::Version::parse("0.1.0").unwrap()),
            min_keyos_version: semver::Version::parse("1.0.0").unwrap(),
            signing_identity: None,
            cosign2_config: None,
            qr_match_rules: Vec::new(),
        }
    }

    #[test]
    fn manifest_from_config_lowercases_app_id_and_falls_back_to_friendly_name() {
        let config = demo_config("0xAABBCCDDEEFF00112233445566778899");
        let permissions: PermissionEntries =
            BTreeMap::from([("os/settings".to_string(), vec!["GetDeviceName".to_string()])]);

        let manifest = app_manifest_from_config(&config, &semver::Version::new(1, 4, 0), permissions);

        assert_eq!(
            manifest.app_name,
            BTreeMap::from([(Locale("en".to_string()), "Demo Friendly".to_string())])
        );
        assert_eq!(
            manifest.app_id,
            [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99]
        );
        assert_eq!(manifest.publisher.as_deref(), Some("Demo Corp"));
        assert_eq!(manifest.description.as_deref(), Some("Demo"));
        assert_eq!(manifest.version, Some(semver::Version::parse("1.4.0").unwrap()));
        assert_eq!(manifest.min_keyos_version, Some(semver::Version::parse("1.0.0").unwrap()));
        assert_eq!(
            manifest.permissions,
            BTreeMap::from([("os/settings".to_string(), BTreeSet::from(["GetDeviceName".to_string()]))])
        );
    }

    #[test]
    fn manifest_from_config_converts_qr_match_rules() {
        let mut config = demo_config("0x00112233445566778899aabbccddeeff");
        config.qr_match_rules.push(QrMatchRuleConfig {
            id: "otpauth".to_string(),
            priority: app_manifest::QrPriority::new(5).unwrap(),
            id_localizations: BTreeMap::from([("en".to_string(), "OTP Auth".to_string())]),
            sub_rules: BTreeMap::from([(
                "qr".to_string(),
                QrMatchSubRuleConfig::QR {
                    min_len: None,
                    max_len: None,
                    regex_pattern: Some("^otpauth://".to_string()),
                },
            )]),
        });

        let manifest =
            app_manifest_from_config(&config, &semver::Version::new(1, 4, 0), PermissionEntries::default());

        assert_eq!(
            manifest.qr_match_rules,
            vec![app_manifest::QrMatchRule {
                id: "otpauth".to_string(),
                priority: app_manifest::QrPriority::new(5).unwrap(),
                id_localizations: BTreeMap::from([(Locale("en".to_string()), "OTP Auth".to_string())]),
                sub_rules: BTreeMap::from([(
                    "qr".to_string(),
                    app_manifest::QrMatchSubRule::QR {
                        min_len: None,
                        max_len: None,
                        regex_pattern: Some("^otpauth://".to_string()),
                    },
                )]),
            }]
        );
    }
}
