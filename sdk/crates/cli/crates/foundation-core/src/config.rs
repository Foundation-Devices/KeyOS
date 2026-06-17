// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Configuration types and parsing

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const APP_CONFIG_FILE: &str = "app-config.toml";
pub const PERMISSION_TEMPLATES_FILE: &str = "permission_templates.toml";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct AppConfig {
    /// Internal application name (used in build artifacts)
    pub app_name: String,

    /// Display name shown to users
    pub friendly_app_name: String,

    /// Shorter name for Launcher UI (falls back to friendly_app_name)
    #[serde(default)]
    pub launcher_app_name: Option<String>,

    /// App description
    pub description: String,

    /// Publisher metadata used for signing certificates and support details.
    #[serde(default)]
    pub publisher: PublisherConfig,

    /// Path to launcher icon (relative to project root)
    pub icon: PathBuf,

    /// Built-in theme id or project-relative theme JSON path.
    #[serde(default)]
    pub theme: Option<String>,

    /// Unique application identifier stored as a 0x-prefixed hex string
    pub app_id: AppId,

    /// Requested permissions in KeyOS manifest format
    #[serde(default)]
    pub permissions: PermissionsConfig,

    /// Semantic version
    pub version: semver::Version,

    /// Minimum supported KeyOS version
    pub min_keyos_version: semver::Version,

    /// Optional publisher signing identity name under ~/.foundation/signing/<identity>
    #[serde(default)]
    pub signing_identity: Option<String>,

    /// Optional path to cosign2.toml
    #[serde(default)]
    pub cosign2_config: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct PublisherConfig {
    #[serde(default)]
    pub name: String,

    #[serde(default)]
    pub contact_email: String,

    #[serde(default)]
    pub support_url: String,
}

impl PublisherConfig {
    pub fn name_value(&self) -> Option<&str> { non_empty_value(&self.name) }

    pub fn contact_email_value(&self) -> Option<&str> { non_empty_value(&self.contact_email) }

    pub fn support_url_value(&self) -> Option<&str> { non_empty_value(&self.support_url) }
}

/// App ID stored as a hex string (e.g., "0x41757468656e74696361746f72203246")
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub struct AppId {
    /// Raw bytes of the app ID
    bytes: Vec<u8>,
    /// Original hex string representation
    hex_string: String,
}

impl AppId {
    /// Create from hex string (e.g., "0x41757468656e74696361746f72203246").
    /// The stored hex string is always normalized to lowercase with a `0x`
    /// prefix so round-tripping config → manifest → comparison is stable
    /// regardless of how the user typed the original.
    pub fn from_hex(hex: &str) -> Result<Self, AppIdError> {
        let hex_body =
            hex.strip_prefix("0x").or_else(|| hex.strip_prefix("0X")).ok_or(AppIdError::MissingPrefix)?;

        if hex_body.is_empty() {
            return Err(AppIdError::Empty);
        }

        if !hex_body.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AppIdError::InvalidHexCharacter);
        }

        if hex_body.len() % 2 != 0 {
            return Err(AppIdError::OddLength);
        }

        let bytes = (0..hex_body.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex_body[i..i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| AppIdError::InvalidHexCharacter)?;

        if bytes.len() != APP_ID_BYTE_LEN {
            return Err(AppIdError::WrongLength { actual: bytes.len() });
        }

        Ok(Self { bytes, hex_string: format!("0x{}", hex_body.to_ascii_lowercase()) })
    }

    /// Get the hex string representation
    pub fn as_hex(&self) -> &str { &self.hex_string }

    /// Get the raw bytes
    pub fn as_bytes(&self) -> &[u8] { &self.bytes }

    /// Try to interpret as UTF-8 string (for display purposes)
    pub fn as_utf8(&self) -> Option<&str> { std::str::from_utf8(&self.bytes).ok() }
}

impl TryFrom<String> for AppId {
    type Error = AppIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> { Self::from_hex(&s) }
}

impl From<AppId> for String {
    fn from(id: AppId) -> String { id.hex_string }
}

impl std::fmt::Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.hex_string) }
}

/// KeyOS app IDs are exactly 16 bytes (32 hex characters). `decode_app_id_str`
/// on the device enforces this, so configs that don't match produce manifests
/// the device rejects at install/launch time. Validate it here instead.
pub const APP_ID_BYTE_LEN: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum AppIdError {
    #[error("App ID must start with '0x' prefix")]
    MissingPrefix,
    #[error("App ID cannot be empty")]
    Empty,
    #[error("App ID contains invalid hex characters")]
    InvalidHexCharacter,
    #[error("App ID hex string must have even length")]
    OddLength,
    #[error("App ID must be exactly {APP_ID_BYTE_LEN} bytes ({} hex characters), got {actual} byte(s)", APP_ID_BYTE_LEN * 2)]
    WrongLength { actual: usize },
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct PermissionsConfig {
    #[serde(default)]
    pub template: Vec<String>,

    #[serde(flatten)]
    pub entries: PermissionEntries,
}

impl AppConfig {
    /// Load and parse app-config.toml from the given path
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::ReadError { path: path.to_owned(), source: e })?;
        let config: Self = toml::from_str(&content)
            .map_err(|e| ConfigError::ParseError { path: path.to_owned(), source: e })?;
        Ok(config)
    }

    /// Validate the config against the project root
    pub fn validate(&self, project_root: &Path) -> Result<(), ConfigError> {
        self.validate_app_name()?;

        let icon_path = project_root.join(&self.icon);
        if !icon_path.exists() {
            return Err(ConfigError::MissingIcon { path: icon_path });
        }
        let _ = self.resolved_permissions(project_root)?;
        Ok(())
    }

    fn validate_app_name(&self) -> Result<(), ConfigError> {
        let name = self.app_name.as_str();
        if name.is_empty() {
            return Err(ConfigError::InvalidAppName {
                name: self.app_name.clone(),
                reason: "must not be empty",
            });
        }

        if name == "." || name == ".." {
            return Err(ConfigError::InvalidAppName {
                name: self.app_name.clone(),
                reason: "must be a plain directory name",
            });
        }

        if Path::new(name).is_absolute() || name.contains('/') || name.contains('\\') {
            return Err(ConfigError::InvalidAppName {
                name: self.app_name.clone(),
                reason: "must not contain path components",
            });
        }

        Ok(())
    }

    /// Get the effective launcher name
    pub fn launcher_name(&self) -> &str {
        self.launcher_app_name.as_deref().unwrap_or(&self.friendly_app_name)
    }

    /// Get the app-manager manifest appName map.
    pub fn manifest_app_names(&self) -> BTreeMap<String, String> {
        let mut app_names = BTreeMap::new();
        app_names.insert("en".to_string(), self.launcher_name().to_string());
        app_names
    }

    /// Get the app-bundle-local path for the converted manifest icon.
    pub fn manifest_icon_file(&self) -> String { "resources/.foundation/icon.raw".to_string() }

    /// Get the manifest icon path without the `.raw` suffix.
    pub fn manifest_icon_image_name(&self) -> String {
        self.manifest_icon_file().trim_end_matches(".raw").to_string()
    }

    /// Expand permission templates and merge them with explicit permission entries.
    pub fn resolved_permissions(&self, project_root: &Path) -> Result<PermissionEntries, ConfigError> {
        self.permissions.resolve(project_root)
    }
}

pub type PermissionEntries = BTreeMap<String, Vec<String>>;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
struct PermissionTemplateLibrary {
    #[serde(flatten)]
    templates: BTreeMap<String, PermissionEntries>,
}

impl PermissionsConfig {
    pub fn resolve(&self, project_root: &Path) -> Result<PermissionEntries, ConfigError> {
        let mut merged = BTreeMap::<String, BTreeSet<String>>::new();

        if !self.template.is_empty() {
            let template_path = project_root.join(PERMISSION_TEMPLATES_FILE);
            let library = load_permission_templates(&template_path)?;

            for template_name in &self.template {
                let template = library.templates.get(template_name).ok_or_else(|| {
                    ConfigError::UnknownPermissionTemplate {
                        name: template_name.clone(),
                        path: template_path.clone(),
                    }
                })?;
                merge_permissions(&mut merged, template);
            }
        }

        merge_permissions(&mut merged, &self.entries);

        Ok(merged.into_iter().map(|(key, values)| (key, values.into_iter().collect())).collect())
    }
}

fn load_permission_templates(path: &Path) -> Result<PermissionTemplateLibrary, ConfigError> {
    let content = std::fs::read_to_string(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ConfigError::MissingPermissionTemplates { path: path.to_path_buf() }
        } else {
            ConfigError::PermissionTemplateReadError { path: path.to_path_buf(), source }
        }
    })?;
    toml::from_str(&content)
        .map_err(|source| ConfigError::PermissionTemplateParseError { path: path.to_path_buf(), source })
}

fn merge_permissions(target: &mut BTreeMap<String, BTreeSet<String>>, source: &PermissionEntries) {
    for (permission, values) in source {
        let entry = target.entry(permission.clone()).or_default();
        for value in values {
            entry.insert(value.clone());
        }
    }
}

fn non_empty_value(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read config file {path}: {source}")]
    ReadError { path: PathBuf, source: std::io::Error },

    #[error("Failed to parse config file {path}: {source}")]
    ParseError { path: PathBuf, source: toml::de::Error },

    #[error("Icon file not found: {path}")]
    MissingIcon { path: PathBuf },

    #[error("Permission templates file not found: {path}")]
    MissingPermissionTemplates { path: PathBuf },

    #[error("Failed to read permission templates {path}: {source}")]
    PermissionTemplateReadError { path: PathBuf, source: std::io::Error },

    #[error("Failed to parse permission templates {path}: {source}")]
    PermissionTemplateParseError { path: PathBuf, source: toml::de::Error },

    #[error("Permission template '{name}' not found in {path}")]
    UnknownPermissionTemplate { name: String, path: PathBuf },

    #[error("Invalid app-name '{name}': {reason}")]
    InvalidAppName { name: String, reason: &'static str },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        AppConfig, AppId, AppIdError, ConfigError, PermissionsConfig, PublisherConfig, APP_CONFIG_FILE,
        PERMISSION_TEMPLATES_FILE,
    };

    #[test]
    fn app_id_accepts_exactly_16_bytes() {
        let id = AppId::from_hex("0x00112233445566778899aabbccddeeff").unwrap();
        assert_eq!(id.as_bytes().len(), 16);
        assert_eq!(id.as_hex(), "0x00112233445566778899aabbccddeeff");
    }

    #[test]
    fn app_id_rejects_short_even_length() {
        // Even length but only 2 bytes — previously accepted, then rejected by
        // the device's decode_app_id_str (needs exactly 32 hex chars).
        assert!(matches!(AppId::from_hex("0x1234"), Err(AppIdError::WrongLength { actual: 2 })));
    }

    #[test]
    fn app_id_rejects_too_long() {
        assert!(matches!(
            AppId::from_hex("0x00112233445566778899aabbccddeeff00"),
            Err(AppIdError::WrongLength { actual: 17 })
        ));
    }

    #[test]
    fn app_id_still_rejects_odd_length_first() {
        assert!(matches!(AppId::from_hex("0x123"), Err(AppIdError::OddLength)));
    }

    #[test]
    fn loads_app_config_and_expands_permission_templates() {
        let root = make_temp_dir("app-config");
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::write(root.join("resources").join("icon.svg"), "<svg />\n").unwrap();
        fs::write(
            root.join(APP_CONFIG_FILE),
            r#"
            app-name = "demo-app"
            friendly-app-name = "Demo App"
            launcher-app-name = "Demo"
            description = "Demo description"
            icon = "resources/icon.svg"
            app-id = "0x00112233445566778899aabbccddeeff"
            version = "0.1.0"
            min-keyos-version = "1.0.0"
            cosign2-config = "~/.foundation/signing/demo-app/cosign2.toml"

            [publisher]
            name = "Demo Corp"
            contact-email = "support@example.com"
            support-url = "https://example.com/support"

            [permissions]
            template = ["gui-app"]
            "os/settings" = ["GetDeviceName"]
            "#,
        )
        .unwrap();
        fs::write(
            root.join(PERMISSION_TEMPLATES_FILE),
            r#"
            [gui-app]
            "os/gui-server" = ["RegisterAppMessage", "RequestRedraw"]
            "os/settings" = ["GetLocale"]
            "#,
        )
        .unwrap();

        let config = AppConfig::load(&root.join(APP_CONFIG_FILE)).unwrap();
        config.validate(&root).unwrap();

        let permissions = config.resolved_permissions(&root).unwrap();
        assert_eq!(permissions["os/gui-server"], vec!["RegisterAppMessage", "RequestRedraw"]);
        assert_eq!(permissions["os/settings"], vec!["GetDeviceName", "GetLocale"]);
        assert_eq!(config.launcher_name(), "Demo");
        assert_eq!(config.manifest_app_names()["en"], "Demo");
        assert_eq!(config.manifest_icon_file(), "resources/.foundation/icon.raw");
        assert_eq!(config.manifest_icon_image_name(), "resources/.foundation/icon");
        assert_eq!(config.theme, None);
        assert_eq!(config.publisher.name_value(), Some("Demo Corp"));
        assert_eq!(config.publisher.contact_email_value(), Some("support@example.com"));
        assert_eq!(config.publisher.support_url_value(), Some("https://example.com/support"));

        cleanup(&root);
    }

    #[test]
    fn validate_rejects_app_names_with_path_components() {
        let root = make_temp_dir("invalid-app-name");
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::write(root.join("resources").join("icon.svg"), "<svg />\n").unwrap();

        for app_name in ["", ".", "..", "../common", "/tmp/demo", "nested/app", r"nested\app"] {
            let config = test_config(app_name);
            assert!(
                matches!(config.validate(&root), Err(ConfigError::InvalidAppName { .. })),
                "app-name {app_name:?} should be rejected"
            );
        }

        cleanup(&root);
    }

    #[test]
    fn validate_fails_when_permission_template_is_missing() {
        let root = make_temp_dir("missing-template");
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::write(root.join("resources").join("icon.svg"), "<svg />\n").unwrap();
        fs::write(
            root.join(APP_CONFIG_FILE),
            r#"
            app-name = "demo-app"
            friendly-app-name = "Demo App"
            description = "Demo description"
            icon = "resources/icon.svg"
            app-id = "0x00112233445566778899aabbccddeeff"
            version = "0.1.0"
            min-keyos-version = "1.0.0"

            [permissions]
            template = ["missing"]
            "#,
        )
        .unwrap();
        fs::write(root.join(PERMISSION_TEMPLATES_FILE), "").unwrap();

        let config = AppConfig::load(&root.join(APP_CONFIG_FILE)).unwrap();
        let error = config.validate(&root).unwrap_err();
        assert!(error.to_string().contains("Permission template 'missing' not found"));

        cleanup(&root);
    }

    fn test_config(app_name: &str) -> AppConfig {
        AppConfig {
            app_name: app_name.to_string(),
            friendly_app_name: "Demo App".to_string(),
            launcher_app_name: None,
            description: "Demo description".to_string(),
            publisher: PublisherConfig::default(),
            icon: PathBuf::from("resources/icon.svg"),
            theme: None,
            app_id: AppId::from_hex("0x00112233445566778899aabbccddeeff").unwrap(),
            permissions: PermissionsConfig::default(),
            version: semver::Version::new(0, 1, 0),
            min_keyos_version: semver::Version::new(1, 0, 0),
            signing_identity: None,
            cosign2_config: None,
        }
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("foundation-app-config-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn cleanup(path: &Path) { let _ = fs::remove_dir_all(path); }
}
