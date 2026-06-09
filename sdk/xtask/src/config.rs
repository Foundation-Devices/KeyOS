// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `sdk-build.toml` loader. Uses serde + `toml` rather than a hand-written
//! parser so quoted-string escapes, multi-line strings, inline tables, and
//! arrays of tables all parse without surprises as the schema grows.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub type DynError = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, DynError>;

// ----- Public, post-validation types ---------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub sdk: SdkConfig,
    pub submodules: BTreeMap<String, SubmoduleConfig>,
    pub targets: TargetsConfig,
    pub compile: Vec<CompileEntry>,
    pub copy: Vec<CopyEntry>,
    pub docs: DocsConfig,
    pub signing: SigningConfig,
}

#[derive(Clone, Debug, Default)]
pub struct SdkConfig {
    pub version: String,
    pub api_version: String,
    pub keyos_api_interfaces: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SubmoduleConfig {
    pub path: String,
    pub repo: String,
    pub r#ref: String,
    pub env_override: String,
}

#[derive(Clone, Debug, Default)]
pub struct TargetsConfig {
    pub triples: Vec<String>,
    pub overrides: BTreeMap<String, TargetOverride>,
}

#[derive(Clone, Debug, Default)]
pub struct TargetOverride {
    pub cross: bool,
    pub linker: String,
}

#[derive(Clone, Debug, Default)]
pub struct CompileEntry {
    pub name: String,
    pub manifest: String,
    pub package: Option<String>,
    pub artifact: Option<String>,
    pub binary: String,
    pub cargo_flags: Vec<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CopyEntry {
    pub source: String,
    pub dest: String,
    pub bundle: CopyBundle,
    pub filter: CopyFilter,
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyBundle {
    #[default]
    Common,
    Target,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyFilter {
    #[default]
    All,
    CargoPackage,
    SlintSdk,
}

#[derive(Clone, Debug, Default)]
pub struct DocsConfig {
    pub guide_source: String,
    pub api_crates: Vec<String>,
    pub api_crates_workspace: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SigningConfig {
    pub key_env: String,
    pub algorithm: String,
}

// ----- Raw TOML schema (deserialized via serde) ----------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    sdk: RawSdk,
    #[serde(default)]
    submodules: BTreeMap<String, RawSubmodule>,
    targets: RawTargets,
    #[serde(default)]
    compile: Vec<RawCompile>,
    #[serde(default)]
    copy: Vec<RawCopy>,
    docs: RawDocs,
    signing: RawSigning,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSdk {
    version: String,
    api_version: String,
    keyos_api_interfaces: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSubmodule {
    path: String,
    repo: String,
    #[serde(rename = "ref")]
    refspec: String,
    env_override: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTargets {
    triples: Vec<String>,
    #[serde(default)]
    overrides: BTreeMap<String, RawTargetOverride>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTargetOverride {
    #[serde(default)]
    cross: bool,
    #[serde(default)]
    linker: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCompile {
    name: String,
    /// Historically called `source`; accepted under either name.
    #[serde(alias = "source")]
    manifest: String,
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    artifact: Option<String>,
    binary: String,
    #[serde(default)]
    cargo_flags: Vec<String>,
    #[serde(default)]
    optional: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCopy {
    source: String,
    dest: String,
    #[serde(default)]
    bundle: CopyBundle,
    #[serde(default)]
    filter: CopyFilter,
    #[serde(default)]
    optional: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDocs {
    guide_source: String,
    #[serde(default)]
    api_crates: Vec<String>,
    #[serde(default)]
    api_crates_workspace: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSigning {
    key_env: String,
    #[serde(default)]
    algorithm: String,
}

// ----- Helpers & loading ---------------------------------------------------

const KEYOS_API_SOURCE_ROOT: &str = "../api";
const KEYOS_API_DEST_ROOT: &str = "lib/keyos/api";

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask has a workspace parent").to_path_buf()
}

pub fn boxed_err(message: impl Into<String>) -> DynError {
    Box::new(Error::new(ErrorKind::Other, message.into()))
}

impl Config {
    pub fn expanded_copy_entries(&self) -> Vec<CopyEntry> {
        let mut entries = self
            .sdk
            .keyos_api_interfaces
            .iter()
            .map(|interface| CopyEntry {
                source: format!("{KEYOS_API_SOURCE_ROOT}/{interface}"),
                dest: format!("{KEYOS_API_DEST_ROOT}/{interface}"),
                bundle: CopyBundle::Common,
                filter: CopyFilter::All,
                optional: false,
            })
            .collect::<Vec<_>>();
        entries.extend(self.copy.clone());
        entries
    }
}

impl From<RawSubmodule> for SubmoduleConfig {
    fn from(raw: RawSubmodule) -> Self {
        SubmoduleConfig { path: raw.path, repo: raw.repo, r#ref: raw.refspec, env_override: raw.env_override }
    }
}

impl From<RawTargetOverride> for TargetOverride {
    fn from(raw: RawTargetOverride) -> Self { TargetOverride { cross: raw.cross, linker: raw.linker } }
}

impl From<RawCompile> for CompileEntry {
    fn from(raw: RawCompile) -> Self {
        CompileEntry {
            name: raw.name,
            manifest: raw.manifest,
            package: raw.package,
            artifact: raw.artifact,
            binary: raw.binary,
            cargo_flags: raw.cargo_flags,
            optional: raw.optional,
        }
    }
}

impl From<RawCopy> for CopyEntry {
    fn from(raw: RawCopy) -> Self {
        CopyEntry {
            source: raw.source,
            dest: raw.dest,
            bundle: raw.bundle,
            filter: raw.filter,
            optional: raw.optional,
        }
    }
}

pub fn load(path: &Path) -> Result<Config> {
    let contents = fs::read_to_string(path)?;
    let raw: RawConfig = toml::from_str(&contents)
        .map_err(|e| boxed_err(format!("failed to parse {}: {e}", path.display())))?;

    let config = Config {
        sdk: SdkConfig {
            version: raw.sdk.version,
            api_version: raw.sdk.api_version,
            keyos_api_interfaces: raw.sdk.keyos_api_interfaces,
        },
        submodules: raw.submodules.into_iter().map(|(k, v)| (k, v.into())).collect(),
        targets: TargetsConfig {
            triples: raw.targets.triples,
            overrides: raw.targets.overrides.into_iter().map(|(k, v)| (k, v.into())).collect(),
        },
        compile: raw.compile.into_iter().map(Into::into).collect(),
        copy: raw.copy.into_iter().map(Into::into).collect(),
        docs: DocsConfig {
            guide_source: raw.docs.guide_source,
            api_crates: raw.docs.api_crates,
            api_crates_workspace: raw.docs.api_crates_workspace,
        },
        signing: SigningConfig { key_env: raw.signing.key_env, algorithm: raw.signing.algorithm },
    };

    validate(&config)?;
    Ok(config)
}

pub fn selected_targets(config: &Config, requested: &[String]) -> Result<Vec<String>> {
    if requested.is_empty() || requested.iter().any(|value| value == "all") {
        return Ok(config.targets.triples.clone());
    }

    for target in requested {
        if !config.targets.triples.iter().any(|item| item == target) {
            return Err(boxed_err(format!("unsupported target triple: {target}")));
        }
    }

    Ok(requested.to_vec())
}

fn validate(config: &Config) -> Result<()> {
    if config.sdk.version.is_empty() {
        return Err(boxed_err("sdk.version is required"));
    }
    if config.sdk.api_version.is_empty() {
        return Err(boxed_err("sdk.api_version is required"));
    }
    if config.sdk.keyos_api_interfaces.is_empty() {
        return Err(boxed_err("sdk.keyos_api_interfaces is required"));
    }
    let mut seen_interfaces = BTreeMap::new();
    for interface in &config.sdk.keyos_api_interfaces {
        if interface.is_empty() {
            return Err(boxed_err("sdk.keyos_api_interfaces cannot contain empty interface names"));
        }
        if seen_interfaces.insert(interface, true).is_some() {
            return Err(boxed_err(format!("sdk.keyos_api_interfaces contains duplicate entry: {interface}")));
        }
    }
    if config.targets.triples.is_empty() {
        return Err(boxed_err("targets.triples is required"));
    }
    if config.compile.is_empty() {
        return Err(boxed_err("at least one [[compile]] entry is required"));
    }
    for entry in &config.compile {
        if entry.name.is_empty() {
            return Err(boxed_err("each [[compile]] entry requires name"));
        }
        if entry.manifest.is_empty() {
            return Err(boxed_err(format!("compile entry '{}' requires manifest", entry.name)));
        }
        if entry.binary.is_empty() {
            return Err(boxed_err(format!("compile entry '{}' requires binary", entry.name)));
        }
    }
    for entry in config.expanded_copy_entries() {
        if entry.source.is_empty() || entry.dest.is_empty() {
            return Err(boxed_err("each [[copy]] entry requires source and dest"));
        }
    }
    if config.docs.guide_source.is_empty() {
        return Err(boxed_err("docs.guide_source is required"));
    }
    if config.signing.key_env.is_empty() {
        return Err(boxed_err("signing.key_env is required"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{load, selected_targets, workspace_root, CopyBundle, CopyFilter};

    #[test]
    fn loads_real_sdk_build_configuration() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();

        assert_eq!(config.sdk.version, "1.0.0");
        assert_eq!(config.sdk.api_version, "1");
        assert_eq!(
            config.sdk.keyos_api_interfaces,
            vec![
                "app-manager".to_string(),
                "crypto".to_string(),
                "fs".to_string(),
                "gui-server".to_string(),
                "haptics".to_string(),
                "quantum-link".to_string(),
                "rgb-led".to_string(),
                "settings".to_string(),
            ]
        );
        assert!(config.submodules.contains_key("slint"));
        assert!(config
            .compile
            .iter()
            .any(|entry| entry.name == "foundation" && entry.binary == "foundation"));
        assert!(config
            .compile
            .iter()
            .any(|entry| entry.name == "foundation-asset-tool" && entry.binary == "foundation-asset-tool"));
        assert!(config
            .compile
            .iter()
            .any(|entry| entry.name == "keyos-log-viewer" && entry.binary == "foundation-keyos-log-viewer"));
        assert!(config
            .compile
            .iter()
            .any(|entry| entry.name == "passport-drive" && entry.binary == "foundation-passport-drive"));
        assert!(config.expanded_copy_entries().iter().any(|entry| entry.dest == "lib/keyos/api/gui-server"));
        assert!(config.expanded_copy_entries().iter().any(|entry| entry.dest == "lib/keyos/api/bt"));
        assert!(config.expanded_copy_entries().iter().any(|entry| entry.dest == "lib/keyos/utils/defer"));
        assert!(config.expanded_copy_entries().iter().any(|entry| entry.dest == "lib/keyos/utils/whence"));
        assert!(config.expanded_copy_entries().iter().any(|entry| entry.dest == "lib/keyos/os/app-manifest"));
        let copy_entries = config.expanded_copy_entries();
        let foundation_themes =
            copy_entries.iter().find(|entry| entry.dest == "lib/keyos/sdk/crates/foundation-themes").unwrap();
        assert_eq!(foundation_themes.bundle, CopyBundle::Common);
        assert_eq!(foundation_themes.filter, CopyFilter::CargoPackage);
        assert!(!copy_entries.iter().any(|entry| entry.source == "ui" || entry.source == "resources"));
        let slint = copy_entries.iter().find(|entry| entry.dest == "lib/slint").unwrap();
        assert_eq!(slint.bundle, CopyBundle::Common);
        assert_eq!(slint.filter, CopyFilter::SlintSdk);
    }

    #[test]
    fn selected_targets_supports_all_or_explicit_entries() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();

        let all = selected_targets(&config, &["all".to_string()]).unwrap();
        assert_eq!(all, config.targets.triples);

        let explicit = selected_targets(
            &config,
            &["aarch64-apple-darwin".to_string(), "x86_64-unknown-linux-gnu".to_string()],
        )
        .unwrap();
        assert_eq!(
            explicit,
            vec!["aarch64-apple-darwin".to_string(), "x86_64-unknown-linux-gnu".to_string()]
        );
    }

    #[test]
    fn selected_targets_rejects_unknown_target() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        let error = selected_targets(&config, &["totally-unknown-target".to_string()]).unwrap_err();
        assert!(error.to_string().contains("unsupported target triple"));
    }

    #[test]
    fn expands_keyos_api_interface_allowlist_into_copy_entries() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        let entries = config.expanded_copy_entries();

        assert!(entries.iter().any(|entry| {
            entry.source == "../api/app-manager" && entry.dest == "lib/keyos/api/app-manager"
        }));
        assert!(entries
            .iter()
            .any(|entry| { entry.source == "../api/settings" && entry.dest == "lib/keyos/api/settings" }));
        assert!(!entries.iter().any(|entry| entry.dest == "lib/keyos/api/backup"));
        assert!(entries.iter().all(|entry| {
            if entry.dest.starts_with("lib/keyos/api/") {
                entry.bundle == CopyBundle::Common && entry.filter == CopyFilter::All
            } else {
                true
            }
        }));
    }

    #[test]
    fn load_parses_copy_bundle_and_filter() {
        let temp_dir = make_temp_dir("config-copy-bundle-filter");
        let config_path = temp_dir.join("sdk-build.toml");
        fs::write(
            &config_path,
            r#"
            [sdk]
            version = "1.0.0"
            api_version = "1"
            keyos_api_interfaces = ["gui-server"]

            [targets]
            triples = ["x86_64-unknown-linux-gnu"]

            [[compile]]
            name = "foundation"
            manifest = "crates/cli"
            binary = "foundation"

            [[copy]]
            source = "crates/foundation-themes"
            dest = "lib/keyos/sdk/crates/foundation-themes"
            bundle = "common"
            filter = "cargo_package"

            [docs]
            guide_source = "docs"
            api_crates = []
            api_crates_workspace = []

            [signing]
            key_env = "FOUNDATION_SIGN_KEY"
            algorithm = "openpgp-detached"
            "#,
        )
        .unwrap();

        let config = load(&config_path).unwrap();
        assert_eq!(config.copy.len(), 1);
        assert_eq!(config.copy[0].bundle, CopyBundle::Common);
        assert_eq!(config.copy[0].filter, CopyFilter::CargoPackage);

        cleanup(&temp_dir);
    }

    #[test]
    fn load_rejects_missing_compile_binary() {
        let temp_dir = make_temp_dir("config-missing-binary");
        let config_path = temp_dir.join("sdk-build.toml");
        fs::write(
            &config_path,
            r#"
            [sdk]
            version = "1.0.0"
            api_version = "1"
            keyos_api_interfaces = ["gui-server"]

            [targets]
            triples = ["x86_64-unknown-linux-gnu"]

            [docs]
            guide_source = "docs"
            api_crates = []
            api_crates_workspace = []

            [signing]
            key_env = "FOUNDATION_SIGN_KEY"
            algorithm = "openpgp-detached"

            [[compile]]
            name = "foundation"
            manifest = "crates/cli"
            "#,
        )
        .unwrap();

        let error = load(&config_path).unwrap_err();
        // Without `binary`, serde rejects the missing required field; with it
        // present-but-empty, validate() would flag "requires binary". Either is
        // acceptable — the user gets a clear message naming the missing field.
        let msg = error.to_string();
        assert!(
            msg.contains("binary") || msg.contains("missing field"),
            "expected error about missing 'binary' field, got: {msg}"
        );

        cleanup(&temp_dir);
    }

    #[test]
    fn load_rejects_unknown_field() {
        let temp_dir = make_temp_dir("config-unknown-field");
        let config_path = temp_dir.join("sdk-build.toml");
        fs::write(
            &config_path,
            r#"
            [sdk]
            version = "1.0.0"
            api_version = "1"
            keyos_api_interfaces = ["gui-server"]
            mystery_field = "oops"

            [targets]
            triples = ["x86_64-unknown-linux-gnu"]

            [docs]
            guide_source = "docs"

            [signing]
            key_env = "K"

            [[compile]]
            name = "foundation"
            manifest = "crates/cli"
            binary = "foundation"
            "#,
        )
        .unwrap();

        let error = load(&config_path).unwrap_err().to_string();
        assert!(
            error.contains("unknown field") || error.contains("mystery_field"),
            "expected unknown-field error, got: {error}"
        );

        cleanup(&temp_dir);
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("foundation-config-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn cleanup(path: &Path) { let _ = fs::remove_dir_all(path); }
}
