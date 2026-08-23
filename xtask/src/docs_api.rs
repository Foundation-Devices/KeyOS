// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Builds the public API rustdoc site and annotates Rust items through a parsed HTML DOM.
//! Regular expressions are limited to Rust type text and rustdoc's search-index data; they are
//! never used to locate or rewrite HTML elements.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use anyhow::{anyhow, bail, ensure, Context, Result};
use app_manifest::{ApiManifest, ApprovalBehavior, Message, RequiredSignature};
use clap::Args;
use dom_query::{Document, Selection};
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::{
    builder::{cargo, project_root},
    TARGET_TRIPLE_KEYOS,
};

#[derive(Args, Debug)]
pub struct DocsApiArgs {
    /// Package this KeyOS version as a deterministic ZIP after building it.
    #[arg(long)]
    package: bool,
    /// KeyOS source checkout to document while retaining this checkout's SDK configuration.
    #[arg(long, value_name = "PATH")]
    source_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DocsWorkspace {
    Keyos,
    Sdk,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrateDoc {
    package: String,
    crate_name: String,
    workspace: DocsWorkspace,
    source: String,
    #[serde(default)]
    dest: Option<String>,
    description: String,
    #[serde(default)]
    permission_manifest: Option<String>,
}

impl CrateDoc {
    fn href(&self) -> String { format!("{}/index.html", self.crate_name) }
}

#[derive(Debug, Deserialize)]
struct SdkBuildConfig {
    sdk: SdkDocsConfig,
}

#[derive(Debug, Deserialize)]
struct SdkDocsConfig {
    version: String,
    api_crates: Vec<CrateDoc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BundleManifest {
    pub(crate) schema_version: u32,
    pub(crate) sdk_version: String,
    pub(crate) current_keyos_version: String,
    pub(crate) default_keyos_version: String,
    pub(crate) versions: Vec<BundleVersion>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BundleVersion {
    pub(crate) keyos_version: String,
    pub(crate) path: String,
    pub(crate) source_revision: String,
    #[serde(default)]
    pub(crate) generator_revision: String,
    pub(crate) source_dirty: bool,
    pub(crate) tree_sha256: String,
    pub(crate) crates: Vec<String>,
}

const FORBIDDEN_CRATES: &[&str] = &["bt", "gpio", "i2c", "spi", "dma", "keyos_api_docs"];
const TEMPLATE_DIR: &str = "xtask/assets/docs-api";
const GENERATED_ASSET_DIR: &str = "foundation-assets";
const BUNDLE_MANIFEST_NAME: &str = "bundle-manifest.json";
const BUNDLE_MANIFEST_SCRIPT_NAME: &str = "bundle-manifest.js";
const BUNDLE_SCHEMA_VERSION: u32 = 1;
// Rustdoc does not inherit Cargo's target rustflags. Keep this aligned with
// .cargo/config.toml's armv7a-unknown-xous-elf rustflags.
const SDK_DOCS_TARGET_FLAGS: &str = "-Zunstable-options --cfg keyos";
const SDK_DOCS_RUST_FLAGS: &str = "--cfg keyos -Zstack-protector=strong -Zunstable-options";
// Private parent/child protocol used by `sdk/xtask build`: the parent holds
// the shared output lock across docs generation and staging, so its child must
// not attempt to acquire the same lock again.
const DOCS_BUNDLE_LOCK_HELD_ENV: &str = "KEYOS_DOCS_BUNDLE_LOCK_HELD";
const SELECTOR_SCRIPT_NAME: &str = "version-selector.js";
const UNAVAILABLE_DOCS_SCRIPT_NAME: &str = "unavailable-docs.js";
const RUSTDOC_SEARCH_RESULT_START: &str = "const addNextResultToOutput=async obj=>{count+=1;";
const RUSTDOC_SEARCH_RESULT_FILTER: &str = "const addNextResultToOutput=async obj=>{while(window.KEYOS_IS_UNAVAILABLE_DOC&&window.KEYOS_IS_UNAVAILABLE_DOC(obj.href)){const skipped=await results.next();if(!skipped.value){await Promise.all(descList);yieldToBrowser().then(()=>{finishedCallback(count,output);});return;}obj=skipped.value;}count+=1;";
const UNAVAILABLE_DOCS_RUNTIME: &str = r#"(function () {
  "use strict";
  var script = document.currentScript;
  var versionRoot = script && script.src ? new URL(".", script.src) : null;
  var unavailable = window.KEYOS_UNAVAILABLE_DOCS || {};
  var crates = new Set(unavailable.crates || []);
  var pages = new Set(unavailable.pages || []);
  var items = new Set(unavailable.items || []);
  window.KEYOS_IS_UNAVAILABLE_DOC = function (href) {
    if (!versionRoot) return false;
    var target;
    try {
      target = new URL(href, window.location.href);
    } catch (_) {
      return false;
    }
    if (target.origin !== versionRoot.origin || target.pathname.indexOf(versionRoot.pathname) !== 0) {
      return false;
    }
    var relative = target.pathname.slice(versionRoot.pathname.length);
    if (items.has(relative + target.hash) || pages.has(relative)) return true;
    return crates.has(relative.split("/", 1)[0]);
  };
})();
"#;
const FOUNDATION: &str = "foundation";
const THIRD_PARTY: &str = "thirdParty";
const NOT_USER_GRANTABLE: &str = "notUserGrantable";
const FOUNDATION_CSS: &str = include_str!("../assets/docs-api/foundation.css");
const FOUNDATION_HEADER: &str = include_str!("../assets/docs-api/header.html");
const TEMPLATE_ASSETS: &[(&str, &str)] = &[
    ("ui/ui/fonts/Montserrat-Light.ttf", "Montserrat-Light.ttf"),
    ("ui/ui/fonts/Montserrat-Regular.ttf", "Montserrat-Regular.ttf"),
    ("ui/ui/fonts/Montserrat-Medium.ttf", "Montserrat-Medium.ttf"),
    ("utils/font-gen/fonts/SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    ("utils/font-gen/fonts/SourceCodePro-SemiBold.ttf", "SourceCodePro-SemiBold.ttf"),
    ("xtask/assets/docs-api/top-logo.webp", "top-logo.webp"),
    ("xtask/assets/docs-api/top-logo-dark.webp", "top-logo-dark.webp"),
];

static MESSAGE_ALLOWED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"MessageAllowed\s*<\s*(?:(?:[A-Za-z_][A-Za-z0-9_]*)::)*([A-Za-z_][A-Za-z0-9_]*)\s*>")
        .expect("MessageAllowed regex is valid")
});

#[derive(Clone, Debug)]
struct MessageDefinition {
    server: String,
    message: Message,
}

type MessageMap = BTreeMap<String, BTreeMap<String, MessageDefinition>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionInfo {
    message: String,
    server: Option<String>,
    permission_group: Option<String>,
    required_signature: Option<String>,
    approval: Option<String>,
    status: PermissionStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum PermissionStatus {
    Known,
    UnknownServer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PermissionRecord {
    #[serde(rename = "crate")]
    crate_name: String,
    html_path: String,
    kind: String,
    item: String,
    item_id: String,
    permissions: Vec<PermissionInfo>,
}

#[derive(Debug, Default)]
struct UnavailableDocs {
    crates: BTreeSet<String>,
    pages: BTreeSet<String>,
    items: BTreeSet<String>,
}

#[derive(Debug)]
struct FilteredDocs {
    published_crates: Vec<String>,
    unavailable: UnavailableDocs,
}

pub fn run(args: DocsApiArgs) -> Result<()> {
    let root = project_root();
    let _docs_bundle_lock =
        if docs_bundle_lock_is_held_by_parent() { None } else { Some(acquire_docs_bundle_lock(&root)?) };
    let source_root = args.source_root.as_deref().unwrap_or(&root);
    let source_root = source_root
        .canonicalize()
        .with_context(|| format!("resolving KeyOS source root {}", source_root.display()))?;
    let config = load_sdk_build_config(&root)?;
    validate_sdk_build_config(&root, &source_root, &config)?;

    let current_keyos_version = crate::KEYOS_VERSION.trim_start_matches('v').to_string();
    validate_source_keyos_version(&source_root, &current_keyos_version)?;
    let bundle_dir = sdk_docs_bundle_dir(&root);
    reset_dir(&bundle_dir)?;
    let destination = bundle_dir.join(version_dir_name(&current_keyos_version));
    let version = build_workspace_version(
        &root,
        &source_root,
        &destination,
        &current_keyos_version,
        &config.sdk.api_crates,
    )?;

    fs::write(bundle_dir.join(SELECTOR_SCRIPT_NAME), include_str!("../assets/docs-api/version-selector.js"))
        .context("writing docs version selector")?;
    let manifest = BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        sdk_version: config.sdk.version,
        current_keyos_version: current_keyos_version.clone(),
        default_keyos_version: current_keyos_version,
        versions: vec![version],
    };
    write_bundle_manifest(&bundle_dir, &manifest)?;
    write_bundle_index(&bundle_dir, &manifest.default_keyos_version)?;
    verify_bundle(&bundle_dir, &manifest)?;

    println!("Built SDK API docs bundle at {}", bundle_dir.join("index.html").display());
    if args.package {
        let archive = package_bundle(&root, &bundle_dir, &manifest.current_keyos_version)?;
        println!("Packaged SDK API docs at {}", archive.display());
    }
    Ok(())
}

fn load_sdk_build_config(root: &Path) -> Result<SdkBuildConfig> {
    let path = root.join("sdk/sdk-build.toml");
    let contents = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&contents).with_context(|| format!("parsing {}", path.display()))
}

fn validate_sdk_build_config(root: &Path, source_root: &Path, config: &SdkBuildConfig) -> Result<()> {
    Version::parse(&config.sdk.version).context("sdk.version must be valid SemVer")?;
    ensure!(!config.sdk.api_crates.is_empty(), "sdk.api_crates must not be empty");

    let sdk_root = root.join("sdk");
    let canonical_source_root =
        source_root.canonicalize().context("resolving the KeyOS source workspace root")?;
    let canonical_sdk_root = sdk_root.canonicalize().context("resolving the SDK workspace root")?;
    let mut packages = BTreeSet::new();
    let mut crate_names = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    for entry in &config.sdk.api_crates {
        ensure!(packages.insert(&entry.package), "duplicate SDK API package: {}", entry.package);
        ensure!(crate_names.insert(&entry.crate_name), "duplicate SDK API crate name: {}", entry.crate_name);
        ensure!(!entry.source.is_empty(), "SDK API package {} has no source", entry.package);
        ensure!(!entry.description.is_empty(), "SDK API package {} has no description", entry.package);
        if let Some(destination) = entry.dest.as_deref() {
            ensure!(
                !destination.is_empty() && destinations.insert(destination),
                "duplicate or empty SDK API destination: {destination}"
            );
        } else {
            ensure!(
                entry.workspace == DocsWorkspace::Sdk,
                "KeyOS API package {} requires an SDK destination",
                entry.package
            );
        }
        ensure!(
            entry.package != "bt"
                && entry.crate_name != "bt"
                && entry.dest.as_deref() != Some("lib/keyos/api/bt"),
            "the real bt API must not be copied or included in SDK API docs"
        );

        let source = crate_source(root, source_root, entry)?;
        let canonical_source = source
            .canonicalize()
            .with_context(|| format!("resolving SDK API source {}", source.display()))?;
        let expected_root = match entry.workspace {
            DocsWorkspace::Keyos => &canonical_source_root,
            DocsWorkspace::Sdk => &canonical_sdk_root,
        };
        ensure!(
            canonical_source.starts_with(expected_root),
            "SDK API source {} is outside its configured workspace",
            source.display()
        );
        ensure!(
            canonical_source.join("Cargo.toml").is_file(),
            "SDK API source {} has no Cargo.toml",
            source.display()
        );

        if let Some(relative_manifest) = entry.permission_manifest.as_deref() {
            ensure!(
                source_root.join(relative_manifest).is_file(),
                "SDK API package {} permission manifest does not exist: {}",
                entry.package,
                relative_manifest
            );
        }
    }

    validate_keyos_version(crate::KEYOS_VERSION.trim_start_matches('v'))?;
    Ok(())
}

fn crate_source(root: &Path, source_root: &Path, entry: &CrateDoc) -> Result<PathBuf> {
    match entry.workspace {
        DocsWorkspace::Sdk => Ok(root.join("sdk").join(&entry.source)),
        DocsWorkspace::Keyos => {
            let relative = Path::new(&entry.source).strip_prefix("..").with_context(|| {
                format!("KeyOS API source must be relative to the SDK workspace: {}", entry.source)
            })?;
            Ok(source_root.join(relative))
        }
    }
}

fn validate_keyos_version(value: &str) -> Result<()> {
    ensure!(
        value.matches('.').count() == 2,
        "the canonical KeyOS version must contain exactly two periods for RecoveryOS compatibility"
    );
    let current = Version::parse(value).context("the canonical KeyOS version must be valid SemVer")?;
    ensure!(current.to_string() == value, "the canonical KeyOS version must use canonical SemVer");
    Ok(())
}

fn validate_source_keyos_version(source_root: &Path, expected: &str) -> Result<()> {
    let path = source_root.join("xtask/src/main.rs");
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("reading canonical KeyOS version from {}", path.display()))?;
    let pattern = Regex::new(r#"(?m)^\s*const\s+KEYOS_VERSION\s*:\s*&str\s*=\s*"([^"]+)"\s*;"#)
        .expect("KeyOS version constant regex is valid");
    let source = pattern
        .captures(&contents)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().trim_start_matches('v'))
        .with_context(|| format!("{} has no canonical KEYOS_VERSION", path.display()))?;
    validate_keyos_version(source)?;
    ensure!(
        source == expected,
        "KeyOS source version {source} does not match docs generator version {expected}"
    );
    Ok(())
}

fn reset_dir(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("removing {}", path.display())),
    }
    fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))
}

fn docs_bundle_lock_path(root: &Path) -> PathBuf { root.join("target/docs-api.lock") }

pub(crate) fn sdk_docs_bundle_dir(root: &Path) -> PathBuf { root.join("target/sdk-docs/api") }

pub(crate) fn acquire_docs_bundle_lock(root: &Path) -> Result<File> {
    let path = docs_bundle_lock_path(root);
    let parent = path.parent().context("docs generation lock has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("creating docs generation lock directory {}", parent.display()))?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening docs generation lock {}", path.display()))?;
    file.lock().with_context(|| format!("waiting for docs generation lock {}", path.display()))?;
    Ok(file)
}

fn build_workspace_version(
    root: &Path,
    source_root: &Path,
    destination: &Path,
    keyos_version: &str,
    crates: &[CrateDoc],
) -> Result<BundleVersion> {
    let target_dir = root.join("target/xtask-docs-api").join(version_dir_name(keyos_version));
    let doc_dir = run_rustdoc(root, source_root, &target_dir, crates)?;
    copy_template_assets(root, &doc_dir)?;
    verify_crate_outputs(&doc_dir, crates)?;
    write_index(&doc_dir, crates)?;
    rewrite_template_paths(&doc_dir)?;
    inject_version_selector(&doc_dir)?;

    let messages = build_message_map(source_root, crates)?;
    let records = annotate_html(&doc_dir, crates, &messages)?;
    let filtered_docs = filter_unavailable_indexes(&doc_dir, crates, &records)?;
    scrub_forbidden_search_paths(&doc_dir, &filtered_docs.unavailable)?;
    assert_no_forbidden_artifacts(&doc_dir)?;
    assert_template_artifacts(&doc_dir, crates)?;
    let _ = fs::remove_file(doc_dir.join(".lock"));

    copy_dir(&doc_dir, destination)?;
    let source_revision = git_revision(source_root)?;
    let generator_revision = git_revision(root)?;
    let source_dirty =
        repository_is_dirty(source_root)? || (source_root != root && repository_is_dirty(root)?);
    let tree_sha256 = directory_sha256(destination)?;
    let published_records = records.iter().filter(|record| !foundation_only(&record.permissions)).count();
    println!("Built KeyOS {keyos_version} API docs with {published_records} SDK function/method entries");
    Ok(BundleVersion {
        keyos_version: keyos_version.to_string(),
        path: format!("{}/", version_dir_name(keyos_version)),
        source_revision,
        generator_revision,
        source_dirty,
        tree_sha256,
        crates: filtered_docs.published_crates,
    })
}

fn run_rustdoc(root: &Path, source_root: &Path, target_dir: &Path, crates: &[CrateDoc]) -> Result<PathBuf> {
    ensure!(
        !source_root.join("api/docs").exists(),
        "api/docs still exists; keyos-api-docs must not be restored"
    );
    ensure!(
        source_root.join("permission_templates.toml").exists(),
        "permission_templates.toml is required by the API proc macros"
    );
    let template_dir = root.join(TEMPLATE_DIR);
    let template_css = template_dir.join("foundation.css");
    let template_header = template_dir.join("header.html");
    ensure!(template_css.exists(), "missing API docs template: {}", template_css.display());
    ensure!(template_header.exists(), "missing API docs template: {}", template_header.display());

    reset_dir(target_dir)?;
    let existing_rustdoc_flags = env::var("RUSTDOCFLAGS").unwrap_or_default();
    let rustdoc_flags =
        custom_target_flags(&compose_rustdoc_flags(&existing_rustdoc_flags, &template_css, &template_header));
    let rust_flags = compiler_target_flags(&env::var("RUSTFLAGS").unwrap_or_default());

    // Build SDK-workspace crates first and KeyOS-workspace crates second. Both
    // use the same target directory and Rust toolchain, so the final Rustdoc
    // assets and search shards cover the configured public crate set.
    for (workspace, directory) in
        [(DocsWorkspace::Sdk, root.join("sdk")), (DocsWorkspace::Keyos, source_root.to_path_buf())]
    {
        let selected = crates.iter().filter(|entry| entry.workspace == workspace).collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let mut command = Command::new(cargo());
        command
            .args([
                "doc",
                "-Z",
                "unstable-options",
                "--no-deps",
                "--target",
                TARGET_TRIPLE_KEYOS,
                "--target-dir",
            ])
            .arg(target_dir);
        for entry in selected {
            command.args(["-p", &entry.package]);
        }
        command.current_dir(&directory).env("RUSTFLAGS", &rust_flags).env("RUSTDOCFLAGS", &rustdoc_flags);
        let status =
            command.status().with_context(|| format!("running cargo doc in {}", directory.display()))?;
        ensure!(status.success(), "cargo doc failed with {status}");
    }

    let doc_dir = rustdoc_output_dir(target_dir);
    ensure!(doc_dir.is_dir(), "cargo doc did not produce {}", doc_dir.display());
    fs::write(
        doc_dir.join("crates.js"),
        format!(
            "window.ALL_CRATES = [{}];\n",
            crates
                .iter()
                .map(|entry| serde_json::to_string(&entry.crate_name))
                .collect::<std::result::Result<Vec<_>, _>>()?
                .join(",")
        ),
    )?;
    Ok(doc_dir)
}

fn rustdoc_output_dir(target_dir: &Path) -> PathBuf { target_dir.join(TARGET_TRIPLE_KEYOS).join("doc") }

fn compose_rustdoc_flags(existing: &str, template_css: &Path, template_header: &Path) -> String {
    let template_flags = format!(
        "--default-theme light --extend-css {} --html-before-content {}",
        template_css.display(),
        template_header.display()
    );
    match existing.trim() {
        "" => template_flags,
        flags => format!("{flags} {template_flags}"),
    }
}

fn custom_target_flags(existing: &str) -> String {
    match existing.trim() {
        "" => SDK_DOCS_TARGET_FLAGS.to_owned(),
        flags => format!("{SDK_DOCS_TARGET_FLAGS} {flags}"),
    }
}

fn compiler_target_flags(existing: &str) -> String {
    match existing.trim() {
        "" => SDK_DOCS_RUST_FLAGS.to_owned(),
        flags => format!("{SDK_DOCS_RUST_FLAGS} {flags}"),
    }
}

fn docs_bundle_lock_is_held_by_parent() -> bool {
    matches!(env::var(DOCS_BUNDLE_LOCK_HELD_ENV).as_deref(), Ok("1"))
}

fn copy_template_assets(root: &Path, doc_dir: &Path) -> Result<()> {
    let destination_dir = doc_dir.join(GENERATED_ASSET_DIR);
    fs::create_dir_all(&destination_dir)
        .with_context(|| format!("creating {}", destination_dir.display()))?;

    for (source, destination) in TEMPLATE_ASSETS {
        let source = root.join(source);
        let destination = destination_dir.join(destination);
        fs::copy(&source, &destination)
            .with_context(|| format!("copying {} to {}", source.display(), destination.display()))?;
    }
    Ok(())
}

fn template_asset_root(doc_dir: &Path, html_path: &Path) -> String {
    let relative_path = html_path.strip_prefix(doc_dir).unwrap_or(html_path);
    let depth = relative_path.parent().map_or(0, |parent| parent.components().count());
    if depth == 0 {
        "./".to_owned()
    } else {
        "../".repeat(depth)
    }
}

fn rewrite_template_paths(doc_dir: &Path) -> Result<()> {
    let mut paths = collect_files(doc_dir)?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "html"))
        .collect::<Vec<_>>();
    paths.sort();

    for path in paths {
        let root_path = template_asset_root(doc_dir, &path);
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let updated = text
            .replace(
                "src=\"./top-logo.webp\"",
                &format!("src=\"{root_path}foundation-assets/top-logo.webp\""),
            )
            .replace(
                "srcset=\"./top-logo-dark.webp\"",
                &format!("srcset=\"{root_path}foundation-assets/top-logo-dark.webp\""),
            )
            .replace("href=\"./index.html\"", &format!("href=\"{root_path}index.html\""));
        if updated != text {
            fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

fn inject_version_selector(doc_dir: &Path) -> Result<()> {
    for path in collect_files(doc_dir)?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "html"))
    {
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        if text.contains(SELECTOR_SCRIPT_NAME) {
            continue;
        }
        let Some(body) = text.rfind("</body>") else { continue };
        let asset_root = template_asset_root(doc_dir, &path);
        let bundle_root = if asset_root == "./" { "../".to_string() } else { format!("../{asset_root}") };
        let tag = format!(
            "<script src=\"{asset_root}{UNAVAILABLE_DOCS_SCRIPT_NAME}\"></script>\n\
             <script src=\"{bundle_root}{BUNDLE_MANIFEST_SCRIPT_NAME}\"></script>\n\
             <script defer src=\"{bundle_root}{SELECTOR_SCRIPT_NAME}\"></script>\n"
        );
        let updated = format!("{}{}{}", &text[..body], tag, &text[body..]);
        fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

fn version_dir_name(version: &str) -> String { format!("v{version}") }

fn artifact_name(version: &str) -> String { format!("keyos-sdk-docs-v{version}.zip") }

fn write_bundle_manifest(bundle_dir: &Path, manifest: &BundleManifest) -> Result<()> {
    let manifest_json = serde_json::to_string_pretty(manifest).context("encoding docs bundle manifest")?;
    fs::write(bundle_dir.join(BUNDLE_MANIFEST_NAME), &manifest_json)
        .context("writing docs bundle manifest")?;
    fs::write(
        bundle_dir.join(BUNDLE_MANIFEST_SCRIPT_NAME),
        format!("window.KEYOS_DOCS_BUNDLE_MANIFEST = {manifest_json};\n"),
    )
    .context("writing local docs bundle manifest script")
}

fn write_bundle_index(bundle_dir: &Path, default_keyos_version: &str) -> Result<()> {
    let target = format!("{}/index.html", version_dir_name(default_keyos_version));
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>Foundation SDK API Documentation</title>\n<link rel=\"canonical\" href=\"{target}\">\n<meta http-equiv=\"refresh\" content=\"0; url={target}\">\n<script>window.location.replace({});</script>\n</head>\n<body><p>Opening <a href=\"{target}\">KeyOS v{}</a> API documentation.</p></body>\n</html>\n",
        serde_json::to_string(&target)?,
        escape_html(default_keyos_version)
    );
    fs::write(bundle_dir.join("index.html"), html).context("writing docs bundle index")
}

fn verify_bundle(bundle_dir: &Path, manifest: &BundleManifest) -> Result<()> {
    ensure!(bundle_dir.join("index.html").is_file(), "docs bundle has no index.html");
    ensure!(bundle_dir.join(SELECTOR_SCRIPT_NAME).is_file(), "docs bundle has no version selector");
    ensure!(
        bundle_dir.join(BUNDLE_MANIFEST_SCRIPT_NAME).is_file(),
        "docs bundle has no local manifest script"
    );
    ensure!(
        manifest.versions.iter().any(|entry| entry.keyos_version == manifest.default_keyos_version),
        "docs bundle default version is absent"
    );
    for entry in &manifest.versions {
        ensure!(
            entry.path == format!("{}/", version_dir_name(&entry.keyos_version)),
            "docs bundle version {} has an invalid path",
            entry.keyos_version
        );
        let version_dir = bundle_dir.join(entry.path.trim_end_matches('/'));
        ensure!(
            version_dir.join("index.html").is_file(),
            "docs bundle version {} has no index",
            entry.keyos_version
        );
        ensure!(
            version_dir.join(UNAVAILABLE_DOCS_SCRIPT_NAME).is_file(),
            "docs bundle version {} has no unavailable-item search filter",
            entry.keyos_version
        );
        ensure!(
            directory_sha256(&version_dir)? == entry.tree_sha256,
            "docs bundle version tree changed after assembly"
        );
    }
    Ok(())
}

fn package_bundle(root: &Path, bundle_dir: &Path, current_keyos_version: &str) -> Result<PathBuf> {
    let destination = root.join("target").join(artifact_name(current_keyos_version));
    let temporary = destination.with_extension("zip.tmp");
    let _ = fs::remove_file(&temporary);
    let mut writer = ZipWriter::new(File::create(&temporary)?);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true)
        .unix_permissions(0o644);
    for path in collect_files(bundle_dir)? {
        let relative = normalized_relative_path(bundle_dir, &path)?;
        writer.start_file(relative, options)?;
        let mut input = File::open(&path)?;
        std::io::copy(&mut input, &mut writer)?;
    }
    let mut file = writer.finish()?;
    file.flush()?;
    file.sync_all()?;
    fs::rename(&temporary, &destination)?;

    let digest = file_sha256(&destination)?;
    let checksum = destination.with_extension("zip.sha256");
    let filename = destination.file_name().and_then(|name| name.to_str()).context("invalid docs ZIP name")?;
    fs::write(checksum, format!("{digest}  {filename}\n"))?;
    Ok(destination)
}

fn copy_dir(source: &Path, destination: &Path) -> Result<()> {
    reset_dir(destination)?;
    for path in collect_files(source)? {
        let relative = path.strip_prefix(source).context("docs file is outside source directory")?;
        let output = destination.join(relative);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&path, &output)
            .with_context(|| format!("copying {} to {}", path.display(), output.display()))?;
    }
    Ok(())
}

fn normalized_relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(root).context("path is outside docs bundle")?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn directory_sha256(root: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for path in collect_files(root)? {
        let relative = normalized_relative_path(root, &path)?;
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        update_tree_entry_header(&mut hasher, &relative, bytes.len() as u64);
        hasher.update(bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn update_tree_entry_header(hasher: &mut Sha256, path: &str, size: u64) {
    hasher.update((path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    hasher.update(size.to_be_bytes());
}

pub(crate) fn file_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn read_bundle_manifest(path: &Path) -> Result<BundleManifest> {
    let contents = fs::read_to_string(path)
        .map_err(|error| anyhow!("cannot read generated manifest {}: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|error| anyhow!("cannot read generated manifest {}: {error}", path.display()))?;
    ensure!(value.is_object(), "generated manifest root is not an object: {}", path.display());
    serde_json::from_value(value)
        .map_err(|error| anyhow!("cannot read generated manifest {}: {error}", path.display()))
}

fn command_output(command: &mut Command) -> Result<String> {
    let output = command.output().context("running command")?;
    ensure!(output.status.success(), "command failed with {}", output.status);
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_revision(root: &Path) -> Result<String> {
    command_output(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(root))
}

fn repository_is_dirty(root: &Path) -> Result<bool> {
    let status = command_output(
        Command::new("git").args(["status", "--porcelain", "--untracked-files=normal"]).current_dir(root),
    )?;
    Ok(!status.is_empty())
}

fn find_static(doc_dir: &Path, prefix: &str, suffix: &str) -> Result<String> {
    let static_dir = doc_dir.join("static.files");
    let mut matches = fs::read_dir(&static_dir)
        .with_context(|| format!("reading {}", static_dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(prefix) && name.ends_with(suffix))
        .collect::<Vec<_>>();
    matches.sort();
    matches.into_iter().next().with_context(|| format!("missing rustdoc asset matching {prefix}*{suffix}"))
}

fn write_index(doc_dir: &Path, crates: &[CrateDoc]) -> Result<()> {
    let normalize_css = find_static(doc_dir, "normalize-", ".css")?;
    let rustdoc_css = find_static(doc_dir, "rustdoc-", ".css")?;
    let noscript_css = find_static(doc_dir, "noscript-", ".css")?;
    let storage_js = find_static(doc_dir, "storage-", ".js")?;
    let main_js = find_static(doc_dir, "main-", ".js")?;
    let search_js = find_static(doc_dir, "search-", ".js")?;
    let stringdex_js = find_static(doc_dir, "stringdex-", ".js")?;
    let settings_js = find_static(doc_dir, "settings-", ".js")?;
    let favicon_png = find_static(doc_dir, "favicon-32x32-", ".png")?;
    let favicon_svg = find_static(doc_dir, "favicon-", ".svg")?;

    let sidebar = crates
        .iter()
        .map(|entry| {
            format!(
                "<li><a href=\"{}\">{}</a></li>",
                escape_html(&entry.href()),
                escape_html(&entry.crate_name)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let table_rows = crates
        .iter()
        .map(|entry| {
            format!(
                "<dt><a class=\"mod\" href=\"{}\" title=\"crate {}\">{}</a></dt><dd>{}</dd>",
                escape_html(&entry.href()),
                escape_html(&entry.crate_name),
                escape_html(&entry.crate_name),
                escape_html(&entry.description)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let index = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta name="generator" content="rustdoc">
<meta name="description" content="KeyOS API documentation">
<title>KeyOS API Docs</title>
<link rel="stylesheet" href="./static.files/{normalize_css}">
<link rel="stylesheet" href="./static.files/{rustdoc_css}">
<link rel="stylesheet" href="./theme.css">
<script id="default-settings" data-use_system_theme="true" data-theme="light"></script>
<meta name="rustdoc-vars" data-root-path="./" data-static-root-path="./static.files/" data-current-crate="" data-themes="" data-resource-suffix="" data-rustdoc-version="generated" data-channel="" data-search-js="{search_js}" data-stringdex-js="{stringdex_js}" data-settings-js="{settings_js}">
<script src="./static.files/{storage_js}"></script>
<script defer src="./crates.js"></script>
<script defer src="./static.files/{main_js}"></script>
<noscript><link rel="stylesheet" href="./static.files/{noscript_css}"></noscript>
<link rel="alternate icon" type="image/png" href="./static.files/{favicon_png}">
<link rel="icon" type="image/svg+xml" href="./static.files/{favicon_svg}">
</head>
<body class="rustdoc mod crate">
{FOUNDATION_HEADER}
<rustdoc-topbar><h2><a href="#">KeyOS API Docs</a></h2></rustdoc-topbar>
<nav class="sidebar">
<div class="sidebar-crate"><h2><a href="./index.html">KeyOS API Docs</a></h2></div>
<div class="sidebar-elems"><section id="rustdoc-toc"><h3><a href="#crates">Crates</a></h3>
<ul class="block">{sidebar}</ul></section></div>
</nav>
<div class="sidebar-resizer" title="Drag to resize sidebar"></div>
<main><div class="width-limiter"><section id="main-content" class="content">
<div class="main-heading"><h1>KeyOS API Docs</h1><rustdoc-toolbar></rustdoc-toolbar></div>
<details class="toggle top-doc" open><summary class="hideme"><span>Expand description</span></summary>
<div class="docblock"><p>Generated rustdoc entry point for the public KeyOS API crates.</p></div></details>
<h2 id="crates" class="section-header">Crates<a href="#crates" class="anchor">&sect;</a></h2>
<dl class="item-table">{table_rows}</dl>
</section></div></main>
</body>
</html>
"##
    );
    fs::write(doc_dir.join("index.html"), index).context("writing rustdoc index")?;
    Ok(())
}

fn build_message_map(root: &Path, crates: &[CrateDoc]) -> Result<MessageMap> {
    let mut by_crate = BTreeMap::new();
    for entry in crates {
        let mut messages = BTreeMap::new();
        if let Some(relative_manifest) = entry.permission_manifest.as_deref() {
            let manifest_path = root.join(relative_manifest);
            let manifest_dir = manifest_path
                .parent()
                .with_context(|| format!("manifest has no parent: {}", manifest_path.display()))?;
            let manifest = ApiManifest::load_with_tracking(manifest_dir, |_| {});
            for (server, server_messages) in manifest.servers {
                for (name, message) in server_messages {
                    ensure!(
                        messages
                            .insert(name.clone(), MessageDefinition { server: server.clone(), message })
                            .is_none(),
                        "message {name} appears in multiple servers for {}",
                        entry.crate_name
                    );
                }
            }
        }
        by_crate.insert(entry.crate_name.to_owned(), messages);
    }
    Ok(by_crate)
}

fn extract_message_allowed(text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    MESSAGE_ALLOWED_RE
        .captures_iter(text)
        .filter_map(|capture| capture.get(1).map(|value| value.as_str().to_owned()))
        .filter(|message| seen.insert(message.clone()))
        .collect()
}

fn resolve_permissions(crate_name: &str, names: &[String], messages: &MessageMap) -> Vec<PermissionInfo> {
    names
        .iter()
        .map(|name| {
            let Some(definition) =
                messages.get(crate_name).and_then(|crate_messages| crate_messages.get(name))
            else {
                return PermissionInfo {
                    message: name.clone(),
                    server: None,
                    permission_group: None,
                    required_signature: None,
                    approval: None,
                    status: PermissionStatus::UnknownServer,
                };
            };

            PermissionInfo {
                message: name.clone(),
                server: Some(definition.server.clone()),
                permission_group: definition.message.permission_group.clone(),
                required_signature: Some(
                    required_signature_name(definition.message.required_signature()).to_owned(),
                ),
                approval: Some(approval_name(definition.message.approval).to_owned()),
                status: PermissionStatus::Known,
            }
        })
        .collect()
}

fn required_signature_name(signature: RequiredSignature) -> &'static str {
    match signature {
        RequiredSignature::ThirdParty => THIRD_PARTY,
        RequiredSignature::Foundation => FOUNDATION,
    }
}

fn approval_name(approval: ApprovalBehavior) -> &'static str {
    match approval {
        ApprovalBehavior::AutoAllow => "autoAllow",
        ApprovalBehavior::GrantOnFirstUse => "grantOnFirstUse",
        ApprovalBehavior::NotUserGrantable => NOT_USER_GRANTABLE,
    }
}

/// A sideloaded app holds neither a Foundation-only permission nor one the device never grants,
/// and it needs every permission an item declares.
fn foundation_only(permissions: &[PermissionInfo]) -> bool {
    permissions.iter().any(|permission| {
        permission.status != PermissionStatus::Known
            || permission.required_signature.as_deref() == Some(FOUNDATION)
            || permission.approval.as_deref() == Some(NOT_USER_GRANTABLE)
    })
}

fn render_permission_block(permissions: &[PermissionInfo]) -> String {
    let content = match permissions {
        [] => "<p><strong>Permission:</strong> not needed</p>".to_owned(),
        [single] => format_permission_content(single),
        many => format!(
            "<ul>{}</ul>",
            many.iter()
                .map(|permission| format!("<li>{}</li>", format_permission_content(permission)))
                .collect::<String>()
        ),
    };
    format!("<div class=\"docblock keyos-permissions\">{content}</div>")
}

fn format_permission_content(permission: &PermissionInfo) -> String {
    let permission_name = if let Some(server) = &permission.server {
        format!("<code>{} / {}</code>", escape_html(server), escape_html(&permission.message))
    } else {
        format!("TBD (<code>MessageAllowed&lt;{}&gt;</code>)", escape_html(&permission.message))
    };
    let group = permission
        .permission_group
        .as_deref()
        .map(|value| format!("<code>{}</code>", escape_html(value)))
        .unwrap_or_else(|| if permission.server.is_some() { "none".to_owned() } else { "TBD".to_owned() });
    let signature = code_or_tbd(permission.required_signature.as_deref());
    let approval = code_or_tbd(permission.approval.as_deref());

    format!(
        "<p><strong>Permission:</strong> {permission_name}</p>\
         <p><strong>Permission group:</strong> {group}</p>\
         <p><strong>Required signature:</strong> {signature}</p>\
         <p><strong>Approval:</strong> {approval}</p>"
    )
}

fn code_or_tbd(value: Option<&str>) -> String {
    value.map(|value| format!("<code>{}</code>", escape_html(value))).unwrap_or_else(|| "TBD".to_owned())
}

fn annotate_html(
    doc_dir: &Path,
    crates: &[CrateDoc],
    messages: &MessageMap,
) -> Result<Vec<PermissionRecord>> {
    let mut records = Vec::new();
    for entry in crates {
        let crate_dir = doc_dir.join(&entry.crate_name);
        if !crate_dir.exists() {
            continue;
        }

        let mut paths = collect_files(&crate_dir)?
            .into_iter()
            .filter(|path| path.extension().is_some_and(|extension| extension == "html"))
            .collect::<Vec<_>>();
        paths.sort();

        for path in paths {
            let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let relative_path = path.strip_prefix(doc_dir).unwrap_or(&path);
            if let Some(updated) =
                annotate_document(&text, &entry.crate_name, relative_path, messages, &mut records)
            {
                fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
            }
        }
    }

    let published_records =
        records.iter().filter(|record| !foundation_only(&record.permissions)).collect::<Vec<_>>();
    fs::write(
        doc_dir.join("keyos-permissions.json"),
        serde_json::to_string_pretty(&published_records).context("serializing permission records")? + "\n",
    )
    .context("writing keyos-permissions.json")?;
    Ok(records)
}

fn annotate_document(
    text: &str,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) -> Option<String> {
    let document = Document::from(text);
    let initial_record_count = records.len();

    annotate_trait_page(&document, crate_name, html_path, messages, records);
    for impl_block in document.select("details.implementors-toggle").iter() {
        annotate_impl_block(&impl_block, crate_name, html_path, messages, records);
    }
    for deref_block in document.select("details.big-toggle").iter() {
        annotate_deref_block(&deref_block, crate_name, html_path, messages, records);
    }
    annotate_function_page(&document, crate_name, html_path, messages, records);

    if records.len() == initial_record_count {
        return None;
    }
    filter_document(&document, &records[initial_record_count..]);
    Some(document.root().inner_html().to_string())
}

/// Records documenting no permission do not decide it either way, and nothing documented is not
/// enough to call a container Foundation-only.
fn all_foundation_only(records: &[PermissionRecord]) -> bool {
    let mut documented = records
        .iter()
        .map(|record| record.permissions.as_slice())
        .filter(|permissions| !permissions.is_empty());
    documented.next().is_some_and(foundation_only) && documented.all(foundation_only)
}

fn filter_document(document: &Document, records: &[PermissionRecord]) {
    for record in records.iter().filter(|record| foundation_only(&record.permissions)) {
        for link in document.select(&format!("nav.sidebar a[href=\"#{}\"]", record.item_id)).iter() {
            let node = *link.nodes().first().expect("selection is non-empty");
            node.parent().filter(|parent| parent.is("li")).unwrap_or(node).remove_from_parent();
        }
    }

    if all_foundation_only(records) {
        document.select("#main-content").remove();
    }
}

fn filter_unavailable_indexes(
    doc_dir: &Path,
    configured_crates: &[CrateDoc],
    records: &[PermissionRecord],
) -> Result<FilteredDocs> {
    let mut pages: BTreeMap<&str, bool> = BTreeMap::new();
    let mut crates: BTreeMap<&str, bool> = BTreeMap::new();
    let mut unavailable = UnavailableDocs::default();
    for record in records.iter().filter(|record| !record.permissions.is_empty()) {
        let reachable = !foundation_only(&record.permissions);
        if !reachable {
            unavailable.items.insert(format!("{}#{}", record.html_path, record.item_id));
        }
        for (index, key) in
            [(&mut pages, record.html_path.as_str()), (&mut crates, record.crate_name.as_str())]
        {
            index.entry(key).and_modify(|value| *value |= reachable).or_insert(reachable);
        }
    }

    let mut by_index: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    unavailable
        .pages
        .extend(pages.iter().filter(|(_, reachable)| !**reachable).map(|(page, _)| (*page).to_owned()));
    for page in unavailable.pages.iter().map(Path::new) {
        let (Some(parent), Some(file)) = (page.parent(), page.file_name().and_then(|name| name.to_str()))
        else {
            continue;
        };
        by_index.entry(doc_dir.join(parent).join("index.html")).or_default().push(file.to_owned());
    }
    unavailable
        .crates
        .extend(crates.into_iter().filter(|(_, reachable)| !reachable).map(|(name, _)| name.to_owned()));
    by_index.insert(
        doc_dir.join("index.html"),
        unavailable.crates.iter().map(|name| format!("{name}/index.html")).collect(),
    );

    for (index, entries) in by_index {
        remove_index_entries(&index, &entries)?;
    }
    remove_unavailable_all_items(doc_dir, &unavailable.pages)?;
    for page in &unavailable.pages {
        if page.split('/').next().is_some_and(|crate_name| unavailable.crates.contains(crate_name)) {
            continue;
        }
        let path = doc_dir.join(page);
        if path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("removing unavailable SDK docs page {}", path.display()))?;
        }
    }
    for name in &unavailable.crates {
        let crate_dir = doc_dir.join(name);
        if crate_dir.exists() {
            fs::remove_dir_all(&crate_dir)
                .with_context(|| format!("removing unavailable SDK docs crate {}", crate_dir.display()))?;
        }
        let source_dir = doc_dir.join("src").join(name);
        if source_dir.exists() {
            fs::remove_dir_all(&source_dir)
                .with_context(|| format!("removing unavailable SDK docs source {}", source_dir.display()))?;
        }
    }
    let published = configured_crates
        .iter()
        .filter(|entry| !unavailable.crates.contains(&entry.crate_name))
        .map(|entry| entry.crate_name.clone())
        .collect::<Vec<_>>();
    fs::write(
        doc_dir.join("crates.js"),
        format!(
            "window.ALL_CRATES = [{}];\n",
            published
                .iter()
                .map(serde_json::to_string)
                .collect::<std::result::Result<Vec<_>, _>>()?
                .join(",")
        ),
    )?;
    Ok(FilteredDocs { published_crates: published, unavailable })
}

fn remove_index_entries(index: &Path, entries: &[String]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    rewrite_document(index, |document| {
        for entry in entries {
            for link in document.select(&format!("dt > a[href=\"{entry}\"]")).iter() {
                let node = *link.nodes().first().expect("selection is non-empty");
                if let Some(item) = node.parent() {
                    let mut sibling = item.next_sibling();
                    while let Some(next) = sibling {
                        sibling = next.next_sibling();
                        if next.is_text() && next.text().trim().is_empty() {
                            next.remove_from_parent();
                            continue;
                        }
                        if next.is("dd") {
                            next.remove_from_parent();
                        }
                        break;
                    }
                    item.remove_from_parent();
                }
            }
            for link in document.select(&format!("nav.sidebar a[href=\"{entry}\"]")).iter() {
                let node = *link.nodes().first().expect("selection is non-empty");
                node.parent().filter(|parent| parent.is("li")).unwrap_or(node).remove_from_parent();
            }
        }
    })
}

fn remove_unavailable_all_items(doc_dir: &Path, unavailable_pages: &BTreeSet<String>) -> Result<()> {
    let mut entries_by_all_page: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for page in unavailable_pages {
        let Some((crate_name, entry)) = page.split_once('/') else { continue };
        entries_by_all_page
            .entry(doc_dir.join(crate_name).join("all.html"))
            .or_default()
            .push(entry.to_owned());
    }
    for (all_page, entries) in entries_by_all_page {
        rewrite_document(&all_page, |document| {
            for entry in &entries {
                for link in document.select(&format!("ul.all-items a[href=\"{entry}\"]")).iter() {
                    let node = *link.nodes().first().expect("selection is non-empty");
                    node.parent().filter(|parent| parent.is("li")).unwrap_or(node).remove_from_parent();
                }
            }
        })?;
    }
    Ok(())
}

fn rewrite_document(path: &Path, edit: impl FnOnce(&Document)) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let document = Document::from(text.as_str());
    edit(&document);
    fs::write(path, document.root().inner_html().to_string())
        .with_context(|| format!("writing {}", path.display()))
}

fn annotate_trait_page(
    document: &Document,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) {
    let Some(file_name) = html_path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    if !file_name.starts_with("trait.") || !file_name.ends_with(".html") {
        return;
    }

    for method in
        document.select(r#"div.methods section[id^="tymethod."], div.methods section[id^="method."]"#).iter()
    {
        if !has_local_source(&method, crate_name) {
            continue;
        }
        let Some(item_id) = method.attr("id").map(|value| value.to_string()) else {
            continue;
        };
        let Some(item) =
            item_id.strip_prefix("tymethod.").or_else(|| item_id.strip_prefix("method.")).map(str::to_owned)
        else {
            continue;
        };
        let permissions = resolve_permissions(crate_name, &extract_message_allowed(&method.text()), messages);
        let method_node = *method.nodes().first().expect("trait method selection is non-empty");
        if foundation_only(&permissions) {
            remove_trait_method(document, method_node, &item_id);
        } else {
            method_node
                .parent()
                .filter(|parent| parent.is("summary"))
                .unwrap_or(method_node)
                .after_html(render_permission_block(&permissions));
        }
        records.push(permission_record(crate_name, html_path, "trait method", &item, &item_id, permissions));
    }
}

fn remove_trait_method(document: &Document, method: dom_query::Node<'_>, item_id: &str) {
    remove_trait_declaration_summary(document, item_id);
    let summary = method.parent().filter(|parent| parent.is("summary"));
    summary.and_then(|summary| summary.parent()).unwrap_or(method).remove_from_parent();
}

fn remove_trait_declaration_summary(document: &Document, item_id: &str) {
    let link = document.select(&format!(r##"pre.rust.item-decl a[href="#{item_id}"]"##)).first();
    let Some(link_node) = link.nodes().first().copied() else {
        return;
    };
    let mut previous = link_node.prev_sibling();
    let mut start = None;
    while let Some(node) = previous {
        if node.is("span.item-spacer") {
            start = Some(node);
            break;
        }
        if node.is_text() {
            let text = node.text().to_string();
            if let Some(offset) = text.rfind('\n') {
                node.set_text(&text[..=offset]);
                start = node.next_sibling();
                break;
            }
        }
        previous = node.prev_sibling();
    }
    let Some(mut node) = start else { return };
    let terminator = if item_id.starts_with("tymethod.") { ";" } else { "{ ... }" };
    loop {
        let next = node.next_sibling();
        if node.is_text() {
            let text = node.text().to_string();
            if let Some(offset) = text.find(terminator) {
                node.set_text(&text[offset + terminator.len()..]);
                break;
            }
        }
        node.remove_from_parent();
        let Some(next) = next else { break };
        node = next;
    }
}

fn annotate_impl_block(
    impl_block: &Selection<'_>,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) {
    let header = impl_block.select("h3.code-header").first();
    if !header.exists() {
        return;
    }

    let header_text = header.text().to_string();
    let is_trait_impl = header_text.contains(" for ");
    if is_trait_impl && is_external_trait(&header) {
        return;
    }
    let impl_messages = extract_message_allowed(&header_text);

    annotate_method_container(
        impl_block,
        &impl_messages,
        !is_trait_impl,
        crate_name,
        html_path,
        messages,
        records,
    );
}

fn annotate_deref_block(
    deref_block: &Selection<'_>,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) {
    if !deref_block.select(r#"h2[id^="deref-methods-"]"#).exists() {
        return;
    }

    annotate_method_container(deref_block, &[], true, crate_name, html_path, messages, records);
}

fn annotate_method_container(
    container: &Selection<'_>,
    inherited_messages: &[String],
    include_without_permissions: bool,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) {
    let first_record = records.len();
    for method in container.select(r#"section[id^="method."]"#).iter() {
        if !has_local_source(&method, crate_name) {
            continue;
        }

        let Some(item_id) = method.attr("id").map(|value| value.to_string()) else {
            continue;
        };
        let Some(item) = item_id.strip_prefix("method.").map(str::to_owned) else {
            continue;
        };

        let mut names = inherited_messages.to_vec();
        for name in extract_message_allowed(&method.text()) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        if names.is_empty() && !include_without_permissions {
            continue;
        }
        let permissions = resolve_permissions(crate_name, &names, messages);
        let method_node = *method.nodes().first().expect("method selection is non-empty");
        let summary = method_node.parent().filter(|parent| parent.is("summary"));
        if foundation_only(&permissions) {
            // Rustdoc wraps a documented method in a toggle and leaves an undocumented one bare.
            let unavailable = summary.and_then(|summary| summary.parent()).unwrap_or(method_node);
            unavailable.remove_from_parent();
        } else {
            summary.unwrap_or(method_node).after_html(render_permission_block(&permissions));
        }

        records.push(permission_record(crate_name, html_path, "method", &item, &item_id, permissions));
    }

    // An impl whose every permission-bearing method is unavailable would otherwise expose a bare
    // implementation header and helper methods that cannot be reached through the public API.
    if all_foundation_only(&records[first_record..]) {
        if let Some(node) = container.nodes().first() {
            node.remove_from_parent();
        }
    }
}

fn annotate_function_page(
    document: &Document,
    crate_name: &str,
    html_path: &Path,
    messages: &MessageMap,
    records: &mut Vec<PermissionRecord>,
) {
    let Some(file_name) = html_path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let Some(item) = file_name.strip_prefix("fn.").and_then(|name| name.strip_suffix(".html")) else {
        return;
    };
    if !has_local_source(&document.select("html"), crate_name) {
        return;
    }

    let declaration = document.select("pre.rust.item-decl").first();
    if !declaration.exists() {
        return;
    }
    let permissions =
        resolve_permissions(crate_name, &extract_message_allowed(&declaration.text()), messages);
    declaration.after_html(render_permission_block(&permissions));
    records.push(permission_record(
        crate_name,
        html_path,
        "function",
        item,
        &format!("fn.{item}"),
        permissions,
    ));
}

fn has_local_source(selection: &Selection<'_>, crate_name: &str) -> bool {
    let path_fragment = format!("src/{crate_name}/");
    selection
        .select("a.src[href]")
        .iter()
        .filter_map(|link| link.attr("href"))
        .any(|href| href.contains(&path_fragment))
}

fn is_external_trait(header: &Selection<'_>) -> bool {
    let Some(header_node) = header.nodes().first() else {
        return false;
    };
    let mut generic_depth = 0usize;
    let mut implemented_trait_href = None;

    // Generic bounds can contain trait links before the implemented trait. Only inspect links at
    // the top level, stopping at `for`, to identify the trait this impl actually implements.
    for node in header_node.descendants_it() {
        if node.is_text() {
            if text_reaches_for_at_top_level(&node.text(), &mut generic_depth) {
                break;
            }
        } else if generic_depth == 0 && node.is("a.trait[href]") {
            implemented_trait_href = node.attr("href");
        }
    }

    implemented_trait_href.is_some_and(|href| href.starts_with("http://") || href.starts_with("https://"))
}

fn text_reaches_for_at_top_level(text: &str, generic_depth: &mut usize) -> bool {
    let mut offset = 0;
    while offset < text.len() {
        if *generic_depth == 0 && text[offset..].starts_with(" for ") {
            return true;
        }

        let character = text[offset..].chars().next().expect("offset is within the string");
        match character {
            '<' => *generic_depth += 1,
            '>' => *generic_depth = generic_depth.saturating_sub(1),
            _ => {}
        }
        offset += character.len_utf8();
    }
    false
}

fn permission_record(
    crate_name: &str,
    html_path: &Path,
    kind: &str,
    item: &str,
    item_id: &str,
    permissions: Vec<PermissionInfo>,
) -> PermissionRecord {
    PermissionRecord {
        crate_name: crate_name.to_owned(),
        html_path: html_path.to_string_lossy().into_owned(),
        kind: kind.to_owned(),
        item: item.to_owned(),
        item_id: item_id.to_owned(),
        permissions,
    }
}

fn scrub_forbidden_search_paths(doc_dir: &Path, unavailable: &UnavailableDocs) -> Result<()> {
    let search_index = doc_dir.join("search.index");
    if search_index.exists() {
        for file in collect_files(&search_index)? {
            let bytes = fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let content = String::from_utf8_lossy(&bytes);
            let mut updated = content.to_string();
            for crate_name in FORBIDDEN_CRATES.iter().copied().filter(|name| *name != "keyos_api_docs") {
                let pattern = Regex::new(&format!(r#""{}(?:::[^"]*)?""#, regex::escape(crate_name)))
                    .expect("forbidden crate regex is valid");
                updated = pattern.replace_all(&updated, "\"internal\"").into_owned();
            }
            if updated != content {
                fs::write(&file, updated).with_context(|| format!("writing {}", file.display()))?;
            }
        }
    }
    write_unavailable_docs_filter(doc_dir, unavailable)?;
    if doc_dir.join("static.files").is_dir() {
        patch_rustdoc_search_results(doc_dir)?;
    }
    Ok(())
}

fn write_unavailable_docs_filter(doc_dir: &Path, unavailable: &UnavailableDocs) -> Result<()> {
    let contents = serde_json::json!({
        "crates": unavailable.crates,
        "pages": unavailable.pages,
        "items": unavailable.items,
    });
    fs::write(
        doc_dir.join(UNAVAILABLE_DOCS_SCRIPT_NAME),
        format!(
            "window.KEYOS_UNAVAILABLE_DOCS = {};\n{UNAVAILABLE_DOCS_RUNTIME}",
            serde_json::to_string(&contents)?
        ),
    )
    .context("writing unavailable SDK docs search filter")
}

fn patch_rustdoc_search_results(doc_dir: &Path) -> Result<()> {
    let search_js = doc_dir.join("static.files").join(find_static(doc_dir, "search-", ".js")?);
    let contents = fs::read_to_string(&search_js)
        .with_context(|| format!("reading rustdoc search runtime {}", search_js.display()))?;
    ensure!(
        contents.matches(RUSTDOC_SEARCH_RESULT_START).count() == 1,
        "rustdoc search runtime changed; cannot safely filter unavailable SDK APIs"
    );
    fs::write(&search_js, contents.replacen(RUSTDOC_SEARCH_RESULT_START, RUSTDOC_SEARCH_RESULT_FILTER, 1))
        .with_context(|| format!("writing rustdoc search runtime {}", search_js.display()))
}

fn assert_no_forbidden_artifacts(doc_dir: &Path) -> Result<()> {
    let forbidden = FORBIDDEN_CRATES.iter().copied().collect::<BTreeSet<_>>();
    let mut failures = BTreeSet::new();

    for path in collect_paths(doc_dir)? {
        let relative = path.strip_prefix(doc_dir).unwrap_or(&path);
        let first = relative.components().next().and_then(|part| part.as_os_str().to_str());
        if first.is_some_and(|part| forbidden.contains(part)) {
            failures.insert(relative.display().to_string());
        }
        let relative_text = relative.to_string_lossy();
        if relative_text.contains("keyos_api_docs") || relative_text.contains("keyos-api-docs") {
            failures.insert(relative_text.into_owned());
        }
    }

    let crates_js = doc_dir.join("crates.js");
    if crates_js.exists() {
        let content = fs::read_to_string(&crates_js).context("reading crates.js")?;
        for crate_name in FORBIDDEN_CRATES {
            if content.contains(&format!("\"{crate_name}\"")) {
                failures.insert(format!("crates.js references {crate_name}"));
            }
        }
    }

    let search_index = doc_dir.join("search.index");
    if search_index.exists() {
        for file in collect_files(&search_index)? {
            let content = String::from_utf8_lossy(&fs::read(&file)?).into_owned();
            for crate_name in FORBIDDEN_CRATES {
                let pattern = Regex::new(&format!(r#""{}(?:::[^"]*)?""#, regex::escape(crate_name)))
                    .expect("forbidden crate regex is valid");
                if pattern.is_match(&content) {
                    failures.insert(format!(
                        "{} references {crate_name}",
                        file.strip_prefix(doc_dir).unwrap_or(&file).display()
                    ));
                }
            }
        }
    }

    if !failures.is_empty() {
        bail!("forbidden rustdoc artifacts found:\n{}", failures.into_iter().collect::<Vec<_>>().join("\n"));
    }
    Ok(())
}

fn assert_template_artifacts(doc_dir: &Path, crates: &[CrateDoc]) -> Result<()> {
    ensure!(
        FOUNDATION_CSS.contains(".foundation-docs-header"),
        "Foundation API docs template is missing its header styles"
    );
    let missing_assets = TEMPLATE_ASSETS
        .iter()
        .map(|(_, destination)| doc_dir.join(GENERATED_ASSET_DIR).join(destination))
        .filter(|path| !path.exists())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    ensure!(missing_assets.is_empty(), "missing API docs template assets: {}", missing_assets.join(", "));

    let theme_css = fs::read_to_string(doc_dir.join("theme.css")).context("reading generated theme CSS")?;
    ensure!(
        theme_css.contains(".foundation-docs-header"),
        "generated theme CSS does not include the Foundation template"
    );

    let index = fs::read_to_string(doc_dir.join("index.html")).context("reading generated docs index")?;
    ensure!(
        index.contains("class=\"foundation-docs-header\""),
        "generated docs index does not include the Foundation header"
    );
    ensure!(
        index.contains("src=\"./foundation-assets/top-logo.webp\""),
        "generated docs index does not link the Foundation logo"
    );

    // Filtering removes crates whose entire surface is Foundation-only. Check
    // a remaining public crate instead of assuming the first configured crate
    // survived that policy pass, and reject a bundle with no public API.
    let entry = crates
        .iter()
        .find(|entry| doc_dir.join(&entry.crate_name).join("index.html").is_file())
        .context("every configured crate was filtered out of the SDK docs")?;
    let crate_index = fs::read_to_string(doc_dir.join(&entry.crate_name).join("index.html"))
        .with_context(|| format!("reading generated {} crate index", entry.crate_name))?;
    ensure!(
        crate_index.contains("class=\"foundation-docs-header\""),
        "generated crate pages do not include the Foundation header"
    );
    ensure!(
        crate_index.contains("src=\"../foundation-assets/top-logo.webp\""),
        "generated crate pages do not link the Foundation logo"
    );
    Ok(())
}

fn verify_crate_outputs(doc_dir: &Path, crates: &[CrateDoc]) -> Result<()> {
    let missing = crates
        .iter()
        .filter(|entry| !doc_dir.join(&entry.crate_name).join("index.html").exists())
        .map(|entry| entry.crate_name.as_str())
        .collect::<Vec<_>>();
    ensure!(missing.is_empty(), "missing rustdoc crate output(s): {}", missing.join(", "));
    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(collect_paths(root)?.into_iter().filter(|path| path.is_file()).collect())
}

fn collect_paths(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }

    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).with_context(|| format!("reading {}", directory.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use app_manifest::MessageType;

    use super::*;

    fn message(
        permission_group: Option<&str>,
        required_signature: Option<RequiredSignature>,
        approval: ApprovalBehavior,
    ) -> Message {
        Message {
            id: 1,
            r#type: MessageType::BlockingScalar,
            description: None,
            cfg: None,
            permission_group: permission_group.map(str::to_owned),
            required_signature,
            approval,
        }
    }

    fn message_map(name: &str, message: Message) -> MessageMap {
        BTreeMap::from([(
            "settings".to_owned(),
            BTreeMap::from([(
                name.to_owned(),
                MessageDefinition { server: "os/settings".to_owned(), message },
            )]),
        )])
    }

    fn build_rustdoc_fixture(test_name: &str) -> PathBuf {
        let output_dir = project_root().join(format!("target/xtask-docs-api-rustdoc-{test_name}"));
        match fs::remove_dir_all(&output_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("removing {}: {error}", output_dir.display()),
        }
        fs::create_dir_all(&output_dir).unwrap();

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docs_api.rs");
        let rustdoc = env::var_os("RUSTDOC").unwrap_or_else(|| "rustdoc".into());
        let output = Command::new(rustdoc)
            .args(["--crate-name", "docs_api_fixture", "--edition", "2021", "--out-dir"])
            .arg(&output_dir)
            .arg(fixture)
            .output()
            .expect("running rustdoc fixture");
        assert!(
            output.status.success(),
            "rustdoc fixture failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output_dir
    }

    #[test]
    fn current_rustdoc_output_is_annotated() {
        let output_dir = build_rustdoc_fixture("annotation");
        let crate_dir = output_dir.join("docs_api_fixture");
        let messages = BTreeMap::from([(
            "docs_api_fixture".to_owned(),
            BTreeMap::from([(
                "GetThing".to_owned(),
                MessageDefinition {
                    server: "os/settings".to_owned(),
                    message: message(Some("settings.read"), None, ApprovalBehavior::AutoAllow),
                },
            )]),
        )]);
        let mut records = Vec::new();

        for file_name in ["struct.Api.html", "trait.LocalTrait.html", "fn.read_free.html"] {
            let path = crate_dir.join(file_name);
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            let relative_path = path.strip_prefix(&output_dir).unwrap();
            let annotated =
                annotate_document(&text, "docs_api_fixture", relative_path, &messages, &mut records)
                    .unwrap_or_else(|| panic!("{} was not annotated", relative_path.display()));
            assert!(annotated.contains("keyos-permissions"));
            assert!(annotated.contains("settings.read"));
            assert!(annotated.contains("thirdParty"));
            assert!(annotated.contains("autoAllow"));
        }

        let items = records.iter().map(|record| record.item.as_str()).collect::<BTreeSet<_>>();
        assert!(items.contains("read"));
        assert!(items.contains("local"));
        assert!(items.contains("provided"));
        assert!(items.contains("inherited"));
        assert!(items.contains("read_free"));
        assert!(!items.contains("plain"));
        assert!(!items.contains("drop"));
        assert!(records
            .iter()
            .filter(|record| ["read", "local", "read_free"].contains(&record.item.as_str()))
            .all(|record| record.permissions.first().is_some_and(|permission| {
                permission.message == "GetThing" && permission.status == PermissionStatus::Known
            })));

        let external_path = crate_dir.join("struct.ExternalDeref.html");
        let external_text = fs::read_to_string(&external_path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", external_path.display()));
        let external_relative = external_path.strip_prefix(&output_dir).unwrap();
        let initial_record_count = records.len();
        assert!(annotate_document(
            &external_text,
            "docs_api_fixture",
            external_relative,
            &messages,
            &mut records,
        )
        .is_none());
        assert_eq!(records.len(), initial_record_count);

        fs::remove_dir_all(output_dir).unwrap();
    }

    #[test]
    fn sdk_docs_remove_items_that_require_a_foundation_signature() {
        let output_dir = build_rustdoc_fixture("foundation-only");
        let path = output_dir.join("docs_api_fixture/struct.Api.html");
        let messages = BTreeMap::from([(
            "docs_api_fixture".to_owned(),
            BTreeMap::from([(
                "GetThing".to_owned(),
                MessageDefinition {
                    server: "os/settings".to_owned(),
                    message: message(None, None, ApprovalBehavior::AutoAllow),
                },
            )]),
        )]);

        let text = fs::read_to_string(&path).unwrap();
        let annotated = annotate_document(
            &text,
            "docs_api_fixture",
            path.strip_prefix(&output_dir).unwrap(),
            &messages,
            &mut Vec::new(),
        )
        .unwrap();

        let document = Document::from(annotated.as_str());
        assert!(!document.select("#main-content").exists());
        assert!(!document.select("nav.sidebar a[href=\"#method.read\"]").exists());
        assert!(!document.select("nav.sidebar a[href=\"#method.local\"]").exists());
        assert!(!annotated.contains("foundation-api-scope"));
        assert!(!annotated.contains("Foundation only"));

        fs::remove_dir_all(output_dir).unwrap();
    }

    #[test]
    fn sdk_docs_filter_foundation_only_trait_declaration_methods() {
        let output_dir = build_rustdoc_fixture("foundation-only-trait-method");
        let path = output_dir.join("docs_api_fixture/trait.LocalTrait.html");
        let messages = BTreeMap::from([(
            "docs_api_fixture".to_owned(),
            BTreeMap::from([(
                "GetThing".to_owned(),
                MessageDefinition {
                    server: "os/settings".to_owned(),
                    message: message(None, None, ApprovalBehavior::AutoAllow),
                },
            )]),
        )]);
        let mut records = Vec::new();

        let text = fs::read_to_string(&path).unwrap();
        let document = Document::from(text.as_str());
        annotate_trait_page(
            &document,
            "docs_api_fixture",
            path.strip_prefix(&output_dir).unwrap(),
            &messages,
            &mut records,
        );
        assert!(records.iter().any(|record| record.item_id == "tymethod.local"));
        assert!(records.iter().any(|record| record.item_id == "tymethod.undocumented"));
        assert!(records.iter().any(|record| record.item_id == "method.provided"));
        assert!(!document.select(r#"[id="tymethod.local"]"#).exists());
        assert!(!document.select(r#"[id="tymethod.undocumented"]"#).exists());
        assert!(!document.select(r#"[id="method.provided"]"#).exists());
        assert!(!document.select(r##"pre.rust.item-decl a[href="#tymethod.local"]"##).exists());
        assert!(!document.select(r##"pre.rust.item-decl a[href="#tymethod.undocumented"]"##).exists());
        assert!(!document.select(r##"pre.rust.item-decl a[href="#method.provided"]"##).exists());
        assert!(!document.root().inner_html().contains("Documented so rustdoc wraps"));

        filter_document(&document, &records);
        assert!(!document.select("nav.sidebar a[href=\"#tymethod.local\"]").exists());
        assert!(!document.select("nav.sidebar a[href=\"#tymethod.undocumented\"]").exists());

        fs::remove_dir_all(output_dir).unwrap();
    }

    #[test]
    fn unavailable_items_are_removed_from_rustdoc_indexes() {
        let fixture = project_root().join("target/xtask-docs-api-index-filter-test");
        let _ = fs::remove_dir_all(&fixture);
        fs::create_dir_all(&fixture).unwrap();
        let index = fixture.join("index.html");
        fs::write(
            &index,
            r#"<html><body><nav class="sidebar"><ul class="block">
               <li><a href="fn.internal.html">internal</a></li>
               <li><a href="fn.public.html">public</a></li>
               </ul></nav><dl>
               <dt><a href="fn.internal.html">internal</a></dt><dd>internal description</dd>
               <dt><a href="fn.public.html">public</a></dt><dd>public description</dd>
               </dl></body></html>"#,
        )
        .unwrap();

        remove_index_entries(&index, &["fn.internal.html".to_owned()]).unwrap();
        let filtered = fs::read_to_string(&index).unwrap();
        assert!(!filtered.contains("fn.internal.html"));
        assert!(!filtered.contains("internal description"));
        assert!(filtered.contains("fn.public.html"));
        assert!(filtered.contains("public description"));

        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn crates_without_a_usable_sdk_api_are_removed_from_the_bundle() {
        let fixture = project_root().join("target/xtask-docs-api-crate-filter-test");
        let _ = fs::remove_dir_all(&fixture);
        for name in ["public", "internal"] {
            fs::create_dir_all(fixture.join(name)).unwrap();
            fs::write(fixture.join(name).join("index.html"), "<html><body></body></html>").unwrap();
        }
        fs::create_dir_all(fixture.join("src/internal")).unwrap();
        fs::write(fixture.join("src/internal/lib.rs.html"), "source").unwrap();
        fs::write(
            fixture.join("index.html"),
            r#"<html><body><dl>
               <dt><a href="public/index.html">public</a></dt>
               <dt><a href="internal/index.html">internal</a></dt>
               </dl></body></html>"#,
        )
        .unwrap();

        let crates = ["public", "internal"].map(|name| CrateDoc {
            package: name.to_owned(),
            crate_name: name.to_owned(),
            workspace: DocsWorkspace::Keyos,
            source: name.to_owned(),
            dest: Some(name.to_owned()),
            description: name.to_owned(),
            permission_manifest: None,
        });
        let records = [
            permission_record(
                "public",
                Path::new("public/struct.Api.html"),
                "method",
                "read",
                "method.read",
                vec![PermissionInfo {
                    message: "Read".to_owned(),
                    server: Some("os/public".to_owned()),
                    permission_group: Some("public.read".to_owned()),
                    required_signature: Some(THIRD_PARTY.to_owned()),
                    approval: Some("autoAllow".to_owned()),
                    status: PermissionStatus::Known,
                }],
            ),
            permission_record(
                "internal",
                Path::new("internal/struct.Api.html"),
                "method",
                "read",
                "method.read",
                vec![PermissionInfo {
                    message: "Read".to_owned(),
                    server: Some("os/internal".to_owned()),
                    permission_group: None,
                    required_signature: Some(FOUNDATION.to_owned()),
                    approval: Some(NOT_USER_GRANTABLE.to_owned()),
                    status: PermissionStatus::Known,
                }],
            ),
        ];

        let filtered_docs = filter_unavailable_indexes(&fixture, &crates, &records).unwrap();
        assert_eq!(filtered_docs.published_crates, ["public"]);
        assert!(fixture.join("public").is_dir());
        assert!(!fixture.join("internal").exists());
        assert!(!fixture.join("src/internal").exists());
        let index = fs::read_to_string(fixture.join("index.html")).unwrap();
        assert!(index.contains("public/index.html"));
        assert!(!index.contains("internal/index.html"));
        assert_eq!(
            fs::read_to_string(fixture.join("crates.js")).unwrap(),
            "window.ALL_CRATES = [\"public\"];\n"
        );

        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn unavailable_pages_are_removed_from_all_indexes_and_search_results() {
        let fixture = project_root().join("target/xtask-docs-api-page-filter-test");
        let _ = fs::remove_dir_all(&fixture);
        fs::create_dir_all(fixture.join("public")).unwrap();
        for page in ["struct.Internal.html", "struct.Public.html"] {
            fs::write(fixture.join("public").join(page), "page").unwrap();
        }
        fs::write(
            fixture.join("public/index.html"),
            r#"<html><body><dl>
               <dt><a href="struct.Internal.html">internal</a></dt><dd>internal description</dd>
               <dt><a href="struct.Public.html">public</a></dt><dd>public description</dd>
               </dl></body></html>"#,
        )
        .unwrap();
        fs::write(
            fixture.join("public/all.html"),
            r#"<html><body><ul class="all-items">
               <li><a href="struct.Internal.html">Internal</a></li>
               <li><a href="struct.Public.html">Public</a></li>
               </ul></body></html>"#,
        )
        .unwrap();

        let crates = [CrateDoc {
            package: "public".to_owned(),
            crate_name: "public".to_owned(),
            workspace: DocsWorkspace::Keyos,
            source: "public".to_owned(),
            dest: Some("public".to_owned()),
            description: "public".to_owned(),
            permission_manifest: None,
        }];
        let records = [
            permission_record(
                "public",
                Path::new("public/struct.Internal.html"),
                "method",
                "internal",
                "method.internal",
                vec![PermissionInfo {
                    message: "Internal".to_owned(),
                    server: Some("os/public".to_owned()),
                    permission_group: None,
                    required_signature: Some(FOUNDATION.to_owned()),
                    approval: Some(NOT_USER_GRANTABLE.to_owned()),
                    status: PermissionStatus::Known,
                }],
            ),
            permission_record(
                "public",
                Path::new("public/struct.Public.html"),
                "method",
                "public",
                "method.public",
                vec![PermissionInfo {
                    message: "Public".to_owned(),
                    server: Some("os/public".to_owned()),
                    permission_group: Some("public.read".to_owned()),
                    required_signature: Some(THIRD_PARTY.to_owned()),
                    approval: Some("autoAllow".to_owned()),
                    status: PermissionStatus::Known,
                }],
            ),
        ];

        let filtered_docs = filter_unavailable_indexes(&fixture, &crates, &records).unwrap();
        scrub_forbidden_search_paths(&fixture, &filtered_docs.unavailable).unwrap();
        assert!(!fixture.join("public/struct.Internal.html").exists());
        assert!(fixture.join("public/struct.Public.html").is_file());
        let index = fs::read_to_string(fixture.join("public/index.html")).unwrap();
        let all_items = fs::read_to_string(fixture.join("public/all.html")).unwrap();
        assert!(!index.contains("struct.Internal.html"));
        assert!(!all_items.contains("struct.Internal.html"));
        assert!(index.contains("struct.Public.html"));
        assert!(all_items.contains("struct.Public.html"));
        let search_filter = fs::read_to_string(fixture.join(UNAVAILABLE_DOCS_SCRIPT_NAME)).unwrap();
        assert!(search_filter.contains("public/struct.Internal.html"));
        assert!(search_filter.contains("method.internal"));

        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn injected_search_filter_is_version_local_and_selector_is_bundle_relative() {
        let fixture = project_root().join("target/xtask-docs-api-script-path-test");
        let _ = fs::remove_dir_all(&fixture);
        fs::create_dir_all(fixture.join("public/module")).unwrap();
        for path in [fixture.join("index.html"), fixture.join("public/module/struct.Item.html")] {
            fs::write(path, "<html><body></body></html>").unwrap();
        }

        inject_version_selector(&fixture).unwrap();

        let root = fs::read_to_string(fixture.join("index.html")).unwrap();
        assert!(root.contains(r#"src="./unavailable-docs.js""#));
        assert!(root.contains(r#"src="../bundle-manifest.js""#));
        assert!(root.contains(r#"src="../version-selector.js""#));
        let nested = fs::read_to_string(fixture.join("public/module/struct.Item.html")).unwrap();
        assert!(nested.contains(r#"src="../../unavailable-docs.js""#));
        assert!(nested.contains(r#"src="../../../bundle-manifest.js""#));
        assert!(nested.contains(r#"src="../../../version-selector.js""#));

        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn rustdoc_search_results_are_filtered_before_rendering() {
        let fixture = project_root().join("target/xtask-docs-api-search-runtime-test");
        let _ = fs::remove_dir_all(&fixture);
        fs::create_dir_all(fixture.join("static.files")).unwrap();
        let search_js = fixture.join("static.files/search-fixture.js");
        fs::write(&search_js, format!("before;{RUSTDOC_SEARCH_RESULT_START}after;")).unwrap();

        patch_rustdoc_search_results(&fixture).unwrap();

        let patched = fs::read_to_string(&search_js).unwrap();
        assert!(patched.contains("window.KEYOS_IS_UNAVAILABLE_DOC(obj.href)"));
        assert!(!patched.contains(RUSTDOC_SEARCH_RESULT_START));
        assert!(patched.starts_with("before;"));
        assert!(patched.ends_with("after;"));

        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn unavailable_search_filter_matches_exact_version_relative_paths() {
        assert!(UNAVAILABLE_DOCS_RUNTIME.contains("new URL(\".\", script.src)"));
        assert!(UNAVAILABLE_DOCS_RUNTIME.contains("items.has(relative + target.hash)"));
        assert!(UNAVAILABLE_DOCS_RUNTIME.contains("pages.has(relative)"));
        assert!(UNAVAILABLE_DOCS_RUNTIME.contains("crates.has(relative.split(\"/\", 1)[0])"));
        assert!(!UNAVAILABLE_DOCS_RUNTIME.contains("endsWith"));
    }

    #[test]
    fn sdk_build_configuration_owns_the_documented_crate_set() {
        let config = load_sdk_build_config(&project_root()).unwrap();
        let root = project_root();
        validate_sdk_build_config(&root, &root, &config).unwrap();

        assert!(config.sdk.api_crates.iter().any(|entry| entry.package == "server"));
        assert!(config.sdk.api_crates.iter().any(|entry| entry.package == "foundation-manifest"));
        assert!(!config.sdk.api_crates.iter().any(|entry| entry.package == "bt" || entry.crate_name == "bt"));
        assert!(!config.sdk.api_crates.iter().any(|entry| entry.package == "keycard"));
    }

    #[test]
    fn keyos_override_changes_sources_without_changing_the_current_sdk_root() {
        let root = Path::new("/current/keyos");
        let source_root = Path::new("/override/keyos");
        let keyos_entry = CrateDoc {
            package: "server".to_owned(),
            crate_name: "server".to_owned(),
            workspace: DocsWorkspace::Keyos,
            source: "../server".to_owned(),
            dest: Some("lib/keyos/server".to_owned()),
            description: "server".to_owned(),
            permission_manifest: None,
        };
        let sdk_entry = CrateDoc {
            package: "foundation-manifest".to_owned(),
            crate_name: "foundation_manifest".to_owned(),
            workspace: DocsWorkspace::Sdk,
            source: "crates/manifest".to_owned(),
            dest: None,
            description: "manifest".to_owned(),
            permission_manifest: None,
        };

        assert_eq!(crate_source(root, source_root, &keyos_entry).unwrap(), source_root.join("server"));
        assert_eq!(crate_source(root, source_root, &sdk_entry).unwrap(), root.join("sdk/crates/manifest"));
    }

    #[test]
    fn keyos_override_must_match_the_generator_version() {
        validate_source_keyos_version(&project_root(), crate::KEYOS_VERSION).unwrap();

        let error = validate_source_keyos_version(&project_root(), "1.4.0-beta1").unwrap_err().to_string();
        assert!(error.contains("does not match docs generator version"), "unexpected error: {error}");
    }

    #[test]
    fn keyos_versions_are_recoveryos_compatible() {
        for version in ["1.4.0", "1.4.0-alpha1", "1.4.0-beta2"] {
            validate_keyos_version(version).unwrap();
        }
        for version in ["1.4.0-alpha.1", "1.4.0-beta.2"] {
            let error = validate_keyos_version(version).unwrap_err().to_string();
            assert!(error.contains("exactly two periods"), "unexpected error: {error}");
        }
    }

    #[test]
    fn directory_digest_includes_paths_and_contents_in_sorted_order() {
        let fixture = project_root().join("target/xtask-docs-api-digest-test");
        let _ = fs::remove_dir_all(&fixture);
        let first = fixture.join("first");
        let second = fixture.join("second");
        for root in [&first, &second] {
            fs::create_dir_all(root.join("nested")).unwrap();
            fs::write(root.join("b"), "two").unwrap();
            fs::write(root.join("nested/a"), "one").unwrap();
        }
        assert_eq!(directory_sha256(&first).unwrap(), directory_sha256(&second).unwrap());
        fs::write(second.join("nested/a"), "changed").unwrap();
        assert_ne!(directory_sha256(&first).unwrap(), directory_sha256(&second).unwrap());
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn release_archive_is_self_contained_for_one_keyos_version() {
        let fixture = project_root().join("target/xtask-docs-api-current-release-test");
        let _ = fs::remove_dir_all(&fixture);
        let bundle = fixture.join("bundle");
        let version_dir = bundle.join("v1.2.4");
        fs::create_dir_all(&version_dir).unwrap();
        fs::write(version_dir.join("index.html"), "1.2.4").unwrap();
        fs::write(bundle.join("index.html"), "bundle index").unwrap();
        fs::write(bundle.join(BUNDLE_MANIFEST_NAME), "{}").unwrap();
        fs::write(bundle.join(BUNDLE_MANIFEST_SCRIPT_NAME), "manifest").unwrap();
        fs::write(bundle.join(SELECTOR_SCRIPT_NAME), "selector").unwrap();
        fs::create_dir_all(fixture.join("target")).unwrap();

        let archive = package_bundle(&fixture, &bundle, "1.2.4").unwrap();
        let mut zip = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
        assert!(zip.by_name("v1.2.4/index.html").is_ok());
        assert!(zip.by_name(BUNDLE_MANIFEST_NAME).is_ok());
        assert!(zip.by_name(BUNDLE_MANIFEST_SCRIPT_NAME).is_ok());
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn bundle_index_redirects_to_the_static_version_index() {
        let bundle = project_root().join("target/xtask-docs-api-bundle-index-test");
        let _ = fs::remove_dir_all(&bundle);
        fs::create_dir_all(&bundle).unwrap();
        write_bundle_index(&bundle, "1.2.4").unwrap();

        let index = fs::read_to_string(bundle.join("index.html")).unwrap();
        assert!(index.contains("v1.2.4/index.html"));
        assert!(!index.contains("url=v1.2.4/\""));
        fs::remove_dir_all(bundle).unwrap();
    }

    #[test]
    fn rustdoc_flags_include_foundation_template() {
        let flags = compose_rustdoc_flags(
            "--cfg docsrs",
            Path::new("/repo/xtask/assets/docs-api/foundation.css"),
            Path::new("/repo/xtask/assets/docs-api/header.html"),
        );

        assert!(flags.starts_with("--cfg docsrs "));
        assert!(flags.contains("--default-theme light"));
        assert!(flags.contains("--extend-css /repo/xtask/assets/docs-api/foundation.css"));
        assert!(flags.contains("--html-before-content /repo/xtask/assets/docs-api/header.html"));
    }

    #[test]
    fn rustdoc_output_uses_the_hardware_target() {
        assert_eq!(
            rustdoc_output_dir(Path::new("target/xtask-docs-api/v1.4.0")),
            Path::new("target/xtask-docs-api/v1.4.0/armv7a-unknown-xous-elf/doc")
        );
    }

    #[test]
    fn rustdoc_flags_enable_the_keyos_hardware_configuration() {
        assert_eq!(custom_target_flags("--cfg docsrs"), "-Zunstable-options --cfg keyos --cfg docsrs");
    }

    #[test]
    fn compiler_flags_preserve_the_hardware_stack_protector() {
        assert_eq!(
            compiler_target_flags("--cfg docsrs"),
            "--cfg keyos -Zstack-protector=strong -Zunstable-options --cfg docsrs"
        );
    }

    #[test]
    fn docs_generation_lock_serializes_shared_outputs() {
        let root = project_root().join("target/xtask-docs-api-lock-test");
        let _ = fs::remove_dir_all(&root);
        let first = acquire_docs_bundle_lock(&root).unwrap();
        let second = OpenOptions::new().read(true).write(true).open(docs_bundle_lock_path(&root)).unwrap();

        let error = second.try_lock().unwrap_err();
        assert!(matches!(error, std::fs::TryLockError::WouldBlock));
        first.unlock().unwrap();
        second.try_lock().unwrap();
        second.unlock().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn foundation_template_assets_are_local_and_packaged() {
        assert!(!FOUNDATION_CSS.contains("@import"));
        assert!(!FOUNDATION_CSS.contains("http://"));
        assert!(!FOUNDATION_CSS.contains("https://"));
        assert!(!FOUNDATION_CSS.contains("../foundation-assets/"));

        let root = project_root();
        let mut destinations = BTreeSet::new();
        for (source, destination) in TEMPLATE_ASSETS {
            assert!(root.join(source).exists(), "missing template source asset: {source}");
            assert!(
                destinations.insert((*destination).to_owned()),
                "duplicate template destination: {destination}"
            );
        }

        let mut referenced = BTreeSet::new();
        let css_url_re = Regex::new(r#"url\(\s*[\"']?([^\"')\s]+)[\"']?\s*\)"#).unwrap();
        for capture in css_url_re.captures_iter(FOUNDATION_CSS) {
            let url = capture.get(1).unwrap().as_str();
            let destination = url
                .strip_prefix("foundation-assets/")
                .unwrap_or_else(|| panic!("CSS asset URL is not local to foundation-assets: {url}"));
            assert!(destinations.contains(destination), "CSS asset is not packaged: {destination}");
            referenced.insert(destination.to_owned());
        }

        let header = Document::from(FOUNDATION_HEADER);
        assert!(!FOUNDATION_HEADER.contains("foundation-api-scope"));
        assert!(!FOUNDATION_HEADER.contains("keyos-api-scope"));
        for (selector, attribute) in [("img[src]", "src"), ("source[srcset]", "srcset")] {
            for element in header.select(selector).iter() {
                let value = element.attr(attribute).unwrap();
                for candidate in value.split(',') {
                    let url = candidate.split_whitespace().next().unwrap();
                    let destination = url
                        .strip_prefix("./")
                        .unwrap_or_else(|| panic!("header asset URL is not local: {url}"));
                    assert!(
                        destinations.contains(destination),
                        "header asset is not packaged: {destination}"
                    );
                    referenced.insert(destination.to_owned());
                }
            }
        }

        assert_eq!(referenced, destinations, "template assets and references differ");
    }

    #[test]
    fn template_asset_paths_follow_html_depth() {
        let root = Path::new("target/doc");
        assert_eq!(template_asset_root(root, &root.join("index.html")), "./");
        assert_eq!(template_asset_root(root, &root.join("settings/index.html")), "../");
        assert_eq!(template_asset_root(root, &root.join("settings/global/enum.SystemTheme.html")), "../../");
    }

    #[test]
    fn ungrouped_messages_show_effective_foundation_policy() {
        let messages = message_map("GetThing", message(None, None, ApprovalBehavior::NotUserGrantable));
        let permissions = resolve_permissions("settings", &["GetThing".to_owned()], &messages);

        assert_eq!(permissions[0].permission_group, None);
        assert_eq!(permissions[0].required_signature.as_deref(), Some("foundation"));
        assert_eq!(permissions[0].approval.as_deref(), Some("notUserGrantable"));
        assert!(render_permission_block(&permissions).contains("Permission group:</strong> none"));
    }

    #[test]
    fn unknown_messages_are_explicitly_marked() {
        let permissions = resolve_permissions("settings", &["Missing".to_owned()], &MessageMap::new());
        assert_eq!(permissions[0].status, PermissionStatus::UnknownServer);
        assert!(foundation_only(&permissions));
        assert!(render_permission_block(&permissions).contains("MessageAllowed&lt;Missing&gt;"));
        assert!(serde_json::to_string(&permissions).unwrap().contains("unknown-server"));
    }

    #[test]
    fn permission_record_preserves_json_contract() {
        let record = permission_record(
            "settings",
            Path::new("settings/fn.read.html"),
            "function",
            "read",
            "fn.read",
            Vec::new(),
        );
        let value = serde_json::to_value(record).unwrap();

        assert_eq!(value["crate"], "settings");
        assert!(value.get("crate_name").is_none());
        assert_eq!(value["html_path"], "settings/fn.read.html");
    }

    #[test]
    fn html_values_are_escaped() {
        assert_eq!(escape_html("<&\"'>"), "&lt;&amp;&quot;&#39;&gt;");
    }
}
