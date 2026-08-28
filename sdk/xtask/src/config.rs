// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `sdk-build.toml` loader. Uses serde + `toml` rather than a hand-written
//! parser so quoted-string escapes, multi-line strings, inline tables, and
//! arrays of tables all parse without surprises as the schema grows.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Error;
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
    pub api_crates: Vec<ApiCrateConfig>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiCrateConfig {
    pub package: String,
    pub crate_name: String,
    pub workspace: ApiWorkspace,
    pub source: String,
    #[serde(default)]
    pub dest: Option<String>,
    pub description: String,
    #[serde(default)]
    pub permission_manifest: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiWorkspace {
    Keyos,
    Sdk,
}

#[derive(Clone, Debug, Default)]
pub struct SubmoduleConfig {
    pub path: String,
    pub repo: String,
    pub r#ref: String,
    pub source_hash: String,
    pub env_override: String,
}

#[derive(Clone, Debug, Default)]
pub struct TargetsConfig {
    pub triples: Vec<String>,
    pub overrides: BTreeMap<String, TargetOverride>,
}

#[derive(Clone, Debug, Default)]
pub struct TargetOverride {
    pub cargo_target: String,
    pub linker: String,
    pub strip: String,
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
}

#[derive(Clone, Debug, Default)]
pub struct SigningConfig {
    pub key_env: String,
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
    api_crates: Vec<ApiCrateConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSubmodule {
    path: String,
    repo: String,
    #[serde(rename = "ref")]
    refspec: String,
    #[serde(default)]
    source_hash: String,
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
    cargo_target: String,
    #[serde(default)]
    linker: String,
    #[serde(default)]
    strip: String,
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSigning {
    key_env: String,
}

// ----- Helpers & loading ---------------------------------------------------

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask has a workspace parent").to_path_buf()
}

pub fn boxed_err(message: impl Into<String>) -> DynError { Box::new(Error::other(message.into())) }

impl Config {
    pub fn expanded_copy_entries(&self) -> Vec<CopyEntry> {
        let mut entries = self
            .sdk
            .api_crates
            .iter()
            .filter_map(|api_crate| api_crate.dest.as_ref().map(|dest| (api_crate, dest)))
            .map(|(api_crate, dest)| CopyEntry {
                source: api_crate.source.clone(),
                dest: dest.clone(),
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
        SubmoduleConfig {
            path: raw.path,
            repo: raw.repo,
            r#ref: raw.refspec,
            source_hash: raw.source_hash,
            env_override: raw.env_override,
        }
    }
}

impl From<RawTargetOverride> for TargetOverride {
    fn from(raw: RawTargetOverride) -> Self {
        TargetOverride { cargo_target: raw.cargo_target, linker: raw.linker, strip: raw.strip }
    }
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
            api_crates: raw.sdk.api_crates,
        },
        submodules: raw.submodules.into_iter().map(|(k, v)| (k, v.into())).collect(),
        targets: TargetsConfig {
            triples: raw.targets.triples,
            overrides: raw.targets.overrides.into_iter().map(|(k, v)| (k, v.into())).collect(),
        },
        compile: raw.compile.into_iter().map(Into::into).collect(),
        copy: raw.copy.into_iter().map(Into::into).collect(),
        docs: DocsConfig { guide_source: raw.docs.guide_source },
        signing: SigningConfig { key_env: raw.signing.key_env },
    };

    validate(&config)?;
    Ok(config)
}

pub fn selected_targets(config: &Config, requested: &[String]) -> Result<Vec<String>> {
    if requested.is_empty() || (requested.len() == 1 && requested[0] == "all") {
        return Ok(config.targets.triples.clone());
    }
    if requested.iter().any(|value| value == "all") {
        return Err(boxed_err("target selector 'all' cannot be combined with other selectors"));
    }

    let mut selected = BTreeSet::new();
    for selector in requested {
        let matches = targets_for_selector(config, selector);
        if matches.is_empty() {
            return Err(boxed_err(format!("target selector '{selector}' matches no configured SDK targets")));
        }
        selected.extend(matches);
    }

    Ok(config.targets.triples.iter().filter(|target| selected.contains(target.as_str())).cloned().collect())
}

fn targets_for_selector(config: &Config, selector: &str) -> Vec<String> {
    if config.targets.triples.iter().any(|target| target == selector) {
        return vec![selector.to_string()];
    }

    let predicate: Option<fn(&str) -> bool> = match selector {
        "mac-all" => Some(|target| target.ends_with("-apple-darwin")),
        "mac-arm" => Some(|target| target.starts_with("aarch64-") && target.ends_with("-apple-darwin")),
        "mac-x86" => Some(|target| target.starts_with("x86_64-") && target.ends_with("-apple-darwin")),
        "linux-all" => Some(|target| target.contains("-linux-")),
        "linux-arm" => Some(|target| target.starts_with("aarch64-") && target.contains("-linux-")),
        "linux-x86" => Some(|target| target.starts_with("x86_64-") && target.contains("-linux-")),
        "win-all" => Some(|target| target.contains("-windows-")),
        "win-arm" => Some(|target| target.starts_with("aarch64-") && target.contains("-windows-")),
        "win-x86" => Some(|target| target.starts_with("x86_64-") && target.contains("-windows-")),
        _ => None,
    };

    predicate
        .map(|matches| config.targets.triples.iter().filter(|target| matches(target)).cloned().collect())
        .unwrap_or_default()
}

fn validate(config: &Config) -> Result<()> {
    if config.sdk.version.is_empty() {
        return Err(boxed_err("sdk.version is required"));
    }
    if config.sdk.api_version.is_empty() {
        return Err(boxed_err("sdk.api_version is required"));
    }
    if config.sdk.api_crates.is_empty() {
        return Err(boxed_err("sdk.api_crates is required"));
    }
    if config.submodules.get("slint").is_some_and(|slint| slint.source_hash.is_empty()) {
        return Err(boxed_err("submodules.slint.source_hash is required"));
    }
    let mut seen_packages = BTreeSet::new();
    let mut seen_crates = BTreeSet::new();
    let mut seen_destinations = BTreeSet::new();
    for api_crate in &config.sdk.api_crates {
        if api_crate.package.is_empty()
            || api_crate.crate_name.is_empty()
            || api_crate.source.is_empty()
            || api_crate.description.is_empty()
        {
            return Err(boxed_err(
                "each [[sdk.api_crates]] entry requires package, crate_name, source, and description",
            ));
        }
        if !seen_packages.insert(&api_crate.package) {
            return Err(boxed_err(format!(
                "sdk.api_crates contains duplicate package: {}",
                api_crate.package
            )));
        }
        if !seen_crates.insert(&api_crate.crate_name) {
            return Err(boxed_err(format!(
                "sdk.api_crates contains duplicate crate_name: {}",
                api_crate.crate_name
            )));
        }
        if api_crate.package == "bt"
            || api_crate.crate_name == "bt"
            || api_crate.dest.as_deref() == Some("lib/keyos/api/bt")
        {
            return Err(boxed_err("the real bt API must not be copied or included in SDK API docs"));
        }
        if let Some(dest) = &api_crate.dest {
            if dest.is_empty() || !seen_destinations.insert(dest) {
                return Err(boxed_err(format!("sdk.api_crates contains an empty or duplicate dest: {dest}")));
            }
        } else if api_crate.workspace == ApiWorkspace::Keyos {
            return Err(boxed_err(format!("KeyOS API package '{}' requires an SDK dest", api_crate.package)));
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
    for entry in &config.copy {
        if entry.dest == "lib/keyos/server" || entry.dest.starts_with("lib/keyos/api/") {
            return Err(boxed_err(format!(
                "public API destination '{}' must be declared in [[sdk.api_crates]], not [[copy]]",
                entry.dest
            )));
        }
    }
    let mut seen_copy_destinations = BTreeSet::new();
    for entry in config.expanded_copy_entries() {
        if entry.source.is_empty() || entry.dest.is_empty() {
            return Err(boxed_err("each [[copy]] entry requires source and dest"));
        }
        if !seen_copy_destinations.insert(entry.dest.clone()) {
            return Err(boxed_err(format!("copy destinations contain a duplicate: {}", entry.dest)));
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

    use super::{load, selected_targets, validate, workspace_root, CopyBundle, CopyFilter};

    #[test]
    fn loads_real_sdk_build_configuration() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();

        assert_eq!(config.sdk.version, "1.0.0");
        assert_eq!(config.sdk.api_version, "1");
        for expected in ["app-manager", "crypto", "fs", "gui-server-api", "settings"] {
            assert!(
                config.sdk.api_crates.iter().any(|api_crate| api_crate.package == expected),
                "sdk-build.toml is missing the {expected} API crate"
            );
        }
        let slint = config.submodules.get("slint").unwrap();
        assert_eq!(slint.source_hash, "sha256-+eriY9l5KFrJFVau27mScWvMemPFx6op5iSI5MvMWBE=");
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
        let slint_viewer = config.compile.iter().find(|entry| entry.name == "slint-viewer").unwrap();
        assert!(slint_viewer.cargo_flags.windows(2).any(|flags| flags == ["--bin", "slint-viewer"]));
        assert!(slint_viewer.cargo_flags.windows(2).any(|flags| flags == ["-p", "i-slint-common"]));
        assert!(slint_viewer
            .cargo_flags
            .iter()
            .any(|flag| flag.contains("i-slint-common/fontconfig-dlopen")));
        let linux_arm = config.targets.overrides.get("aarch64-unknown-linux-gnu").unwrap();
        assert_eq!(linux_arm.cargo_target, "aarch64-unknown-linux-musl");
        assert_eq!(linux_arm.linker, "aarch64-unknown-linux-musl-gcc");
        assert_eq!(linux_arm.strip, "aarch64-unknown-linux-musl-strip");
        assert!(config.expanded_copy_entries().iter().any(|entry| entry.dest == "lib/keyos/api/gui-server"));
        assert!(!config.expanded_copy_entries().iter().any(|entry| entry.dest == "lib/keyos/api/bt"));
        assert!(config.expanded_copy_entries().iter().any(|entry| entry.dest == "lib/keyos/server"));
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
    fn selected_targets_supports_platform_aliases_in_config_order() {
        let mut config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        config
            .targets
            .triples
            .extend(["x86_64-pc-windows-msvc".to_string(), "aarch64-pc-windows-msvc".to_string()]);

        assert_eq!(
            selected_targets(&config, &["mac-all".to_string()]).unwrap(),
            ["aarch64-apple-darwin", "x86_64-apple-darwin"]
        );
        assert_eq!(
            selected_targets(&config, &["linux-all".to_string()]).unwrap(),
            ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]
        );
        assert_eq!(
            selected_targets(&config, &["win-all".to_string()]).unwrap(),
            ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]
        );
        assert_eq!(
            selected_targets(&config, &["mac-arm".to_string(), "linux-x86".to_string()]).unwrap(),
            ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
        );
    }

    #[test]
    fn windows_aliases_are_reserved_until_windows_targets_are_configured() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        let error = selected_targets(&config, &["win-all".to_string()]).unwrap_err();
        assert!(error.to_string().contains("matches no configured SDK targets"));
    }

    #[test]
    fn selected_targets_rejects_unknown_target() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        let error = selected_targets(&config, &["totally-unknown-target".to_string()]).unwrap_err();
        assert!(error.to_string().contains("matches no configured SDK targets"));
    }

    #[test]
    fn selected_targets_rejects_all_mixed_with_other_selectors() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        let error = selected_targets(&config, &["all".to_string(), "mac-arm".to_string()]).unwrap_err();
        assert!(error.to_string().contains("cannot be combined"));
    }

    #[test]
    fn expands_public_api_crates_into_copy_entries() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        let entries = config.expanded_copy_entries();

        for api_crate in config.sdk.api_crates.iter().filter(|api_crate| api_crate.dest.is_some()) {
            let dest = api_crate.dest.as_ref().unwrap();
            let entry = entries
                .iter()
                .find(|entry| &entry.dest == dest)
                .unwrap_or_else(|| panic!("missing copy entry for public API package {}", api_crate.package));
            assert_eq!(entry.source, api_crate.source);
            assert_eq!(entry.bundle, CopyBundle::Common);
            assert_eq!(entry.filter, CopyFilter::All);
        }
    }

    #[test]
    fn rejects_bt_and_parallel_public_api_copy_configuration() {
        let mut config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        config.sdk.api_crates[0].package = "bt".to_string();
        assert!(validate(&config).unwrap_err().to_string().contains("real bt API must not be copied"));

        let mut config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        config.copy.push(super::CopyEntry {
            source: "../api/extra".to_string(),
            dest: "lib/keyos/api/extra".to_string(),
            ..Default::default()
        });
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("must be declared in [[sdk.api_crates]]"));
    }

    #[test]
    fn load_parses_copy_bundle_and_filter() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("sdk-build.toml");
        fs::write(
            &config_path,
            r#"
            [sdk]
            version = "1.0.0"
            api_version = "1"

            [[sdk.api_crates]]
            package = "gui-server-api"
            crate_name = "gui_server_api"
            workspace = "keyos"
            source = "../api/gui-server"
            dest = "lib/keyos/api/gui-server"
            description = "GUI API"

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

            [signing]
            key_env = "FOUNDATION_SIGN_KEY"
            "#,
        )
        .unwrap();

        let config = load(&config_path).unwrap();
        assert_eq!(config.copy.len(), 1);
        assert_eq!(config.copy[0].bundle, CopyBundle::Common);
        assert_eq!(config.copy[0].filter, CopyFilter::CargoPackage);
    }

    #[test]
    fn load_rejects_missing_compile_binary() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("sdk-build.toml");
        fs::write(
            &config_path,
            r#"
            [sdk]
            version = "1.0.0"
            api_version = "1"

            [[sdk.api_crates]]
            package = "gui-server-api"
            crate_name = "gui_server_api"
            workspace = "keyos"
            source = "../api/gui-server"
            dest = "lib/keyos/api/gui-server"
            description = "GUI API"

            [targets]
            triples = ["x86_64-unknown-linux-gnu"]

            [docs]
            guide_source = "docs"

            [signing]
            key_env = "FOUNDATION_SIGN_KEY"

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
    }

    #[test]
    fn load_rejects_unknown_field() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("sdk-build.toml");
        fs::write(
            &config_path,
            r#"
            [sdk]
            version = "1.0.0"
            api_version = "1"
            mystery_field = "oops"

            [[sdk.api_crates]]
            package = "gui-server-api"
            crate_name = "gui_server_api"
            workspace = "keyos"
            source = "../api/gui-server"
            dest = "lib/keyos/api/gui-server"
            description = "GUI API"

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
    }
}
