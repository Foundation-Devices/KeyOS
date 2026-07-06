// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Manifest conversion from SDK app config to the shared KeyOS manifest schema.

use std::collections::{BTreeMap, BTreeSet};

use app_manifest::{Locale, Manifest};

use crate::config::{AppConfig, PermissionEntries};

pub type AppManifest = Manifest;

pub fn app_manifest_from_config(config: &AppConfig, permissions: PermissionEntries) -> AppManifest {
    AppManifest {
        app_name: config
            .manifest_app_names()
            .into_iter()
            .map(|(locale, name)| (Locale(locale), name))
            .collect(),
        app_id: config.app_id.as_bytes().try_into().expect("AppConfig validation enforces 16-byte app IDs"),
        publisher: config.publisher.name_value().map(ToOwned::to_owned),
        description: (!config.description.trim().is_empty()).then(|| config.description.clone()),
        version: Some(config.version.to_string()),
        servers: BTreeMap::new(),
        fixed_sids: BTreeMap::new(),
        permissions: permissions_to_sets(permissions),
        memory: Vec::new(),
        syscall: Vec::new(),
        qr_match_rules: Vec::new(),
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
    use crate::config::{AppConfig, AppId, PermissionEntries, PermissionsConfig, PublisherConfig};

    #[test]
    fn manifest_from_config_lowercases_app_id_and_falls_back_to_friendly_name() {
        let config = AppConfig {
            app_name: "demo-app".to_string(),
            friendly_app_name: "Demo Friendly".to_string(),
            launcher_app_name: None,
            description: "Demo".to_string(),
            publisher: PublisherConfig { name: "Demo Corp".to_string(), ..Default::default() },
            icon: PathBuf::from("resources/icon.svg"),
            theme: None,
            app_id: AppId::from_hex("0xAABBCCDDEEFF00112233445566778899").unwrap(),
            permissions: PermissionsConfig::default(),
            version: semver::Version::parse("0.1.0").unwrap(),
            min_keyos_version: semver::Version::parse("1.0.0").unwrap(),
            signing_identity: None,
            cosign2_config: None,
        };
        let permissions: PermissionEntries =
            BTreeMap::from([("os/settings".to_string(), vec!["GetDeviceName".to_string()])]);

        let manifest = app_manifest_from_config(&config, permissions);

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
        assert_eq!(manifest.version.as_deref(), Some("0.1.0"));
        assert_eq!(
            manifest.permissions,
            BTreeMap::from([("os/settings".to_string(), BTreeSet::from(["GetDeviceName".to_string()]))])
        );
    }
}
