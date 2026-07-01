// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! manifest.json types

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, PermissionEntries};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppManifest {
    pub app_name: BTreeMap<String, String>,
    pub app_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub version: String,
    #[serde(default)]
    pub permissions: PermissionEntries,
}

impl AppManifest {
    pub fn from_config(config: &AppConfig, permissions: PermissionEntries) -> Self {
        Self {
            app_name: config.manifest_app_names(),
            app_id: config.app_id.as_hex().to_lowercase(),
            publisher: config.publisher.name_value().map(ToOwned::to_owned),
            description: (!config.description.trim().is_empty()).then(|| config.description.clone()),
            version: config.version.to_string(),
            permissions,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::AppManifest;
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

        let manifest = AppManifest::from_config(&config, permissions.clone());

        assert_eq!(manifest.app_name, BTreeMap::from([("en".to_string(), "Demo Friendly".to_string())]));
        assert_eq!(manifest.app_id, "0xaabbccddeeff00112233445566778899");
        assert_eq!(manifest.publisher.as_deref(), Some("Demo Corp"));
        assert_eq!(manifest.description.as_deref(), Some("Demo"));
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.permissions, permissions);
    }
}
