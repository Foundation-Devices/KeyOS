// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT OR Apache-2.0

#[cfg(not(keyos))]
use {crate::ApiManifest, std::path::Path};
use {crate::Manifest, serde::Deserialize};

pub mod manifest_v0;
pub use manifest_v0::{ApiManifestV0, ManifestV0, QrMatchRuleV0, QrMatchSubRuleV0, QrPriorityV0};

// HOW TO ADD A NEW VERSION
// ========================
// When introducing manifest version N (a breaking schema change):
//
// 1. Stop editing `manifest_v(N-1).rs`. It is now a frozen historical record.
//
// 2. Create `schema/manifest_vN.rs` with the new `ManifestVN` and `ApiManifestVN` structs, and `impl
//    From<ManifestV(N-1)> for ManifestVN` (and likewise for ApiManifest). That is the only impl needed in the
//    version file — one per version. Do NOT add load methods or accessors here; those live in lib.rs (see
//    step 4).
//
// 3. In `ManifestSchemas` and `ApiManifestSchemas` below: a. Add a `VN(ManifestVN)` variant with
//    `#[serde(rename = "N")]`. b. In `step()`, change the V(N-1) arm from `Ok(v)` to
//    `Err(Self::VN(ManifestVN::from(v)))`. c. Add a new arm `Self::VN(v) => Ok(v)`.
//
// 4. In lib.rs: update the type aliases. The `impl Manifest` / `impl ApiManifest` blocks below the aliases
//    require no changes — they follow the aliases automatically.

/// Holds a manifest at any supported schema version.
/// Serde dispatches on the `"version"` string tag; `step()` advances one version at a time.
#[derive(Debug, Deserialize)]
#[serde(tag = "manifestVersion")]
enum ManifestSchemas {
    #[serde(rename = "0")]
    V0(ManifestV0),
}

impl ManifestSchemas {
    /// Advance one schema version. Returns `Ok(Manifest)` when the current version is reached,
    /// or `Err(Self)` with the next intermediate version to continue stepping.
    fn step(self) -> Result<Manifest, Self> {
        match self {
            Self::V0(v) => Ok(v), // ManifestV0 is current — update when bumping the type alias
        }
    }

    fn into_latest(self) -> Manifest {
        let mut current = self;
        loop {
            match current.step() {
                Ok(manifest) => return manifest,
                Err(next) => current = next,
            }
        }
    }
}

/// Holds an API manifest at any supported schema version.
#[cfg(not(keyos))]
#[derive(Debug, Deserialize)]
#[serde(tag = "manifestVersion")]
enum ApiManifestSchemas {
    #[serde(rename = "0")]
    V0(ApiManifestV0),
}

#[cfg(not(keyos))]
impl ApiManifestSchemas {
    fn step(self) -> Result<ApiManifest, Self> {
        match self {
            Self::V0(v) => Ok(v),
        }
    }

    fn into_latest(self) -> ApiManifest {
        let mut current = self;
        loop {
            match current.step() {
                Ok(manifest) => return manifest,
                Err(next) => current = next,
            }
        }
    }
}

/// Parse and migrate JSON manifest bytes to the current schema version.
/// Missing `manifestVersion` is treated as `"0"`.
pub fn migrate_json(bytes: &[u8]) -> Result<Manifest, serde_json::Error> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
    if let Some(obj) = value.as_object_mut() {
        obj.entry("manifestVersion").or_insert("0".into());
    }
    Ok(serde_json::from_value::<ManifestSchemas>(value)?.into_latest())
}

/// Parse and migrate a TOML server manifest to the current schema version.
/// Missing `manifestVersion` is treated as `"0"`.
/// Panics on failure — intended for build-time use only.
#[cfg(not(keyos))]
pub fn migrate_server_toml(content: &str, crate_dir: &Path) -> Manifest {
    let any = parse_toml_manifest_schemas(content, crate_dir);
    any.into_latest()
}

/// Parse and migrate a TOML API manifest to the current schema version.
/// Missing `manifestVersion` is treated as `"0"`.
/// Panics on failure — intended for build-time use only.
#[cfg(not(keyos))]
pub fn migrate_api_toml(content: &str, crate_dir: &Path) -> ApiManifest {
    let any = parse_toml_api_manifest_schemas(content, crate_dir);
    any.into_latest()
}

#[cfg(not(keyos))]
fn parse_toml_manifest_schemas(content: &str, crate_dir: &Path) -> ManifestSchemas {
    let mut value: toml::Value = toml::from_str(content)
        .unwrap_or_else(|e| panic!("Failed to parse manifest at {crate_dir:?}: {e:?}"));
    if let toml::Value::Table(ref mut table) = value {
        table.entry("manifestVersion").or_insert(toml::Value::String("0".into()));
    }
    ManifestSchemas::deserialize(value)
        .unwrap_or_else(|e| panic!("Failed to deserialize manifest at {crate_dir:?}: {e:?}"))
}

#[cfg(not(keyos))]
fn parse_toml_api_manifest_schemas(content: &str, crate_dir: &Path) -> ApiManifestSchemas {
    let mut value: toml::Value = toml::from_str(content)
        .unwrap_or_else(|e| panic!("Failed to parse API manifest at {crate_dir:?}: {e:?}"));
    if let toml::Value::Table(ref mut table) = value {
        table.entry("manifestVersion").or_insert(toml::Value::String("0".into()));
    }
    ApiManifestSchemas::deserialize(value)
        .unwrap_or_else(|e| panic!("Failed to deserialize API manifest at {crate_dir:?}: {e:?}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::Locale;

    fn make_v0() -> ManifestV0 {
        ManifestV0 {
            app_name: BTreeMap::from([(Locale("en".into()), "Test".into())]),
            app_id: [
                0xbf, 0x5c, 0xdf, 0xbf, 0xda, 0x7e, 0x85, 0xb5, 0x25, 0x3f, 0xf2, 0x68, 0xd3, 0x2e, 0xa9,
                0x57,
            ],
            publisher: None,
            description: None,
            version: None,
            min_keyos_version: None,
            servers: Default::default(),
            fixed_sids: Default::default(),
            permissions: Default::default(),
            memory: Default::default(),
            syscall: Default::default(),
            qr_match_rules: Default::default(),
            file_hashes: Default::default(),
        }
    }

    fn make_api_v0() -> ApiManifestV0 { ApiManifestV0 { extends: None, servers: Default::default() } }

    #[test]
    fn manifest_into_latest_terminates() { ManifestSchemas::V0(make_v0()).into_latest(); }

    #[test]
    fn api_manifest_into_latest_terminates() { ApiManifestSchemas::V0(make_api_v0()).into_latest(); }

    const VALID_APP_ID: &str = "0xbf5cdfbfda7e85b5253ff268d32ea957";

    #[cfg(not(keyos))]
    mod toml_tests {
        use std::path::Path;

        use super::*;

        fn v0_toml(extra: &str) -> String {
            format!("manifestVersion = \"0\"\nappId = \"{VALID_APP_ID}\"\n[appName]\nen = \"Test\"\n{extra}")
        }

        #[test]
        fn toml_with_manifest_version_parses_successfully() {
            let manifest = migrate_server_toml(&v0_toml(""), Path::new("."));
            assert_eq!(manifest.app_name_en(), "Test");
        }

        #[test]
        fn toml_missing_manifest_version_defaults_to_v0() {
            let toml = format!("appId = \"{VALID_APP_ID}\"\n[appName]\nen = \"Test\"\n");
            let manifest = migrate_server_toml(&toml, Path::new("."));
            assert_eq!(manifest.app_name_en(), "Test");
        }

        #[test]
        #[should_panic(expected = "unknown variant")]
        fn toml_unknown_manifest_version_panics() {
            migrate_server_toml(&v0_toml("").replace("\"0\"", "\"99\""), Path::new("."));
        }
    }
}
