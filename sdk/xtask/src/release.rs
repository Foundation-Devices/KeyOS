// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::config::{boxed_err, selected_targets, Config, Result};
use crate::package::{self, common_archive_name, target_archive_name, WrittenReleaseMetadata};
use crate::util;

pub const PUBLIC_DOWNLOAD_ROOT: &str = "https://sdk.foundation.xyz";
const DEFAULT_BUCKET: &str = "gs://foundation-sdk";
const RELEASE_MANIFEST_NAME: &str = "release.toml";

#[derive(Clone, Debug)]
pub struct FinalizeArgs {
    pub targets: Vec<String>,
    pub version: Option<String>,
    pub keyos_version: Option<String>,
    pub output_dir: PathBuf,
    pub sign_key: Option<String>,
    pub verbose: bool,
}

impl Default for FinalizeArgs {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            version: None,
            keyos_version: None,
            output_dir: PathBuf::from("dist"),
            sign_key: None,
            verbose: false,
        }
    }
}

impl FinalizeArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut args = Self::default();
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--target" => args.targets.push(next_value(&mut iter, "--target")?),
                "--version" => args.version = Some(next_value(&mut iter, "--version")?),
                "--keyos-version" => args.keyos_version = Some(next_value(&mut iter, "--keyos-version")?),
                "--output-dir" => args.output_dir = PathBuf::from(next_value(&mut iter, "--output-dir")?),
                "--sign-key" => args.sign_key = Some(next_value(&mut iter, "--sign-key")?),
                "--verbose" => args.verbose = true,
                other if other.starts_with('-') => {
                    return Err(boxed_err(format!("unsupported finalize option: {other}")));
                }
                selector => args.targets.push(selector.to_string()),
            }
        }

        Ok(args)
    }
}

#[derive(Clone, Debug)]
pub struct UploadArgs {
    pub release: String,
    pub output_dir: PathBuf,
    pub bucket: String,
    pub link_as_latest: bool,
    pub dry_run: bool,
    pub verbose: bool,
}

impl UploadArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut release = None;
        let mut output_dir = PathBuf::from("dist");
        let mut bucket = env::var("FOUNDATION_SDK_GCS_BUCKET").unwrap_or_else(|_| DEFAULT_BUCKET.to_string());
        let mut link_as_latest = false;
        let mut dry_run = false;
        let mut verbose = false;
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--output-dir" => output_dir = PathBuf::from(next_value(&mut iter, "--output-dir")?),
                "--bucket" => bucket = next_value(&mut iter, "--bucket")?,
                "--link-as-latest" => link_as_latest = true,
                "--dry-run" => dry_run = true,
                "--verbose" => verbose = true,
                other if other.starts_with('-') => {
                    return Err(boxed_err(format!("unsupported upload option: {other}")));
                }
                value if release.is_none() => release = Some(value.to_string()),
                value => return Err(boxed_err(format!("unexpected upload argument: {value}"))),
            }
        }

        let release = release.ok_or_else(|| boxed_err("upload requires a release such as v1.0.0"))?;
        if !bucket.starts_with("gs://") {
            return Err(boxed_err("upload bucket must be a gs:// URL"));
        }

        Ok(Self {
            release,
            output_dir,
            bucket: bucket.trim_end_matches('/').to_string(),
            link_as_latest,
            dry_run,
            verbose,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleaseManifest {
    format_version: u32,
    sdk_version: String,
    keyos_version: String,
    release: String,
    base_url: String,
    targets: Vec<String>,
    workspace_commit: String,
    target_workspace_commits: BTreeMap<String, String>,
    signing_fingerprint: String,
    files: Vec<ReleaseFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleaseFile {
    name: String,
    sha256: String,
    size: u64,
    kind: String,
}

pub fn validated_sdk_version(root: &Path, config: &Config, requested: Option<&str>) -> Result<String> {
    let configured = Version::parse(&config.sdk.version)
        .map_err(|error| boxed_err(format!("sdk.version must be valid SemVer: {error}")))?;

    for (label, path) in [
        ("SDK workspace", root.join("Cargo.toml")),
        ("foundation CLI workspace", root.join("crates/cli/Cargo.toml")),
    ] {
        let version = workspace_package_version(&path)?;
        if version != configured {
            return Err(boxed_err(format!(
                "{label} version {version} does not match sdk-build.toml version {configured}"
            )));
        }
    }

    if let Some(requested) = requested {
        let requested = Version::parse(requested.trim_start_matches('v')).map_err(|error| {
            boxed_err(format!("requested SDK version '{requested}' is not valid SemVer: {error}"))
        })?;
        if requested != configured {
            return Err(boxed_err(format!(
                "requested SDK version {requested} does not match sdk-build.toml version {configured}"
            )));
        }
    }

    Ok(configured.to_string())
}

pub fn validated_keyos_version(config: &Config, requested: Option<&str>) -> Result<String> {
    let requested = requested
        .ok_or_else(|| boxed_err("finalize requires a KeyOS API version; use --keyos-version VERSION"))?;
    let normalized = requested.strip_prefix('v').unwrap_or(requested);
    if normalized.matches('.').count() != 2 {
        return Err(boxed_err(
            "KeyOS versions must contain exactly two periods for RecoveryOS compatibility",
        ));
    }
    let requested = Version::parse(normalized).map_err(|error| {
        boxed_err(format!("requested KeyOS version '{requested}' is not valid SemVer: {error}"))
    })?;
    if requested.to_string() != normalized {
        return Err(boxed_err("KeyOS versions must use canonical SemVer"));
    }

    let configured = Version::parse(&config.sdk.keyos_version)
        .map_err(|error| boxed_err(format!("sdk.keyos_version must be valid SemVer: {error}")))?;
    if requested != configured {
        return Err(boxed_err(format!(
            "requested KeyOS version {requested} does not match sdk-build.toml version {configured}"
        )));
    }
    Ok(requested.to_string())
}

fn workspace_package_version(path: &Path) -> Result<Version> {
    let contents =
        fs::read_to_string(path).map_err(|error| boxed_err(format!("read {}: {error}", path.display())))?;
    let document: toml::Value =
        toml::from_str(&contents).map_err(|error| boxed_err(format!("parse {}: {error}", path.display())))?;
    let value = document
        .get("workspace")
        .and_then(|value| value.get("package"))
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| boxed_err(format!("{} has no workspace.package.version", path.display())))?;
    Version::parse(value)
        .map_err(|error| boxed_err(format!("invalid workspace version in {}: {error}", path.display())))
}

fn canonical_release_label(version: &Version) -> Result<String> {
    if !version.build.is_empty() {
        return Err(boxed_err("SDK release versions cannot contain SemVer build metadata"));
    }
    Ok(format!("v{version}"))
}

fn version_from_release_label(label: &str) -> Result<Version> {
    let raw =
        label.strip_prefix('v').ok_or_else(|| boxed_err(format!("release '{label}' must begin with 'v'")))?;
    let version =
        Version::parse(raw).map_err(|error| boxed_err(format!("invalid release '{label}': {error}")))?;
    let canonical = canonical_release_label(&version)?;
    if canonical != label {
        return Err(boxed_err(format!("release '{label}' is not canonical; use '{canonical}'")));
    }
    Ok(version)
}

pub fn finalize(root: &Path, config: &Config, args: &FinalizeArgs) -> Result<()> {
    ensure_current_checkout_clean(root)?;
    let version_text = validated_sdk_version(root, config, args.version.as_deref())?;
    let keyos_version = validated_keyos_version(config, args.keyos_version.as_deref())?;
    let version = Version::parse(&version_text)?;
    let release = canonical_release_label(&version)?;
    let targets = selected_targets(config, &args.targets)?;
    let output_dir = util::absolute_path(root, &args.output_dir);
    let releases_dir = output_dir.join("releases");
    let release_dir = releases_dir.join(&release);

    let sign_key =
        args.sign_key.clone().or_else(|| package::default_sign_key(config)).ok_or_else(|| {
            boxed_err(format!("finalize requires --sign-key or {}", config.signing.key_env))
        })?;
    package::check_finalize_prerequisites(Some(&sign_key))?;
    fs::create_dir_all(&releases_dir)?;

    let staging = releases_dir.join(format!(".{release}.tmp-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir(&staging)?;

    let result = finalize_into(
        root,
        &output_dir,
        &staging,
        &version_text,
        &keyos_version,
        &release,
        &targets,
        &sign_key,
        args.verbose,
    );
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    let replaced = replace_finalized_release(&staging, &release_dir)?;
    println!(
        "{} SDK {version_text} at {}",
        if replaced { "re-finalized" } else { "finalized" },
        release_dir.display()
    );
    Ok(())
}

fn replace_finalized_release(staging: &Path, release_dir: &Path) -> Result<bool> {
    if !release_dir.exists() {
        fs::rename(staging, release_dir)?;
        return Ok(false);
    }
    if !release_dir.is_dir() {
        return Err(boxed_err(format!(
            "finalized release path is not a directory: {}",
            release_dir.display()
        )));
    }

    let parent = release_dir.parent().ok_or_else(|| {
        boxed_err(format!("finalized release has no parent directory: {}", release_dir.display()))
    })?;
    let release_name = release_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| boxed_err(format!("invalid finalized release path: {}", release_dir.display())))?;
    let backup = (0..1000)
        .map(|index| parent.join(format!(".{release_name}.previous-{}-{index}", std::process::id())))
        .find(|path| !path.exists())
        .ok_or_else(|| boxed_err(format!("could not allocate backup path for {}", release_dir.display())))?;

    fs::rename(release_dir, &backup)?;
    if let Err(promote_error) = fs::rename(staging, release_dir) {
        if let Err(restore_error) = fs::rename(&backup, release_dir) {
            return Err(boxed_err(format!(
                "could not promote new finalized release ({promote_error}) or restore {} ({restore_error})",
                backup.display()
            )));
        }
        return Err(promote_error.into());
    }
    if let Err(error) = fs::remove_dir_all(&backup) {
        eprintln!(
            "warning: re-finalization succeeded, but could not remove backup {}: {error}",
            backup.display()
        );
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn finalize_into(
    root: &Path,
    source_dir: &Path,
    staging: &Path,
    version: &str,
    keyos_version: &str,
    release: &str,
    targets: &[String],
    sign_key: &str,
    verbose: bool,
) -> Result<()> {
    let common_name = common_archive_name(version);
    let common_source = source_dir.join(&common_name);
    require_file(&common_source, "packaged common archive")?;
    let common_archive = staging.join(&common_name);
    util::copy_file(&common_source, &common_archive)?;

    let tar_program = package::find_gnu_tar()?.ok_or_else(|| boxed_err("GNU tar is required"))?;
    let current_commit = git_head(root)?;
    validate_common_archive(&tar_program, &common_archive, version, keyos_version, &current_commit)?;
    let mut archive_paths = vec![common_archive];
    let mut target_workspace_commits = BTreeMap::new();
    for target in targets {
        let name = target_archive_name(version, target);
        let source = source_dir.join(&name);
        require_file(&source, &format!("packaged archive for {target}"))?;
        let destination = staging.join(&name);
        util::copy_file(&source, &destination)?;
        let target_commit =
            validate_target_archive(&tar_program, &destination, version, keyos_version, target)?;
        target_workspace_commits.insert(target.clone(), target_commit);
        archive_paths.push(destination);
    }

    let base_url = format!("{PUBLIC_DOWNLOAD_ROOT}/{release}");
    let written = package::write_release_metadata(
        staging,
        version,
        targets,
        &archive_paths,
        Some(sign_key),
        &base_url,
        verbose,
    )?;
    validate_written_metadata(staging, version, targets, &base_url, &written)?;
    smoke_install_native(staging, version, targets, verbose)?;
    write_release_manifest(
        staging,
        version,
        keyos_version,
        release,
        &base_url,
        targets,
        &current_commit,
        &target_workspace_commits,
        &written,
    )?;
    validate_release_directory(staging, version, keyos_version, release)?;
    Ok(())
}

fn require_file(path: &Path, label: &str) -> Result<()> {
    if !path.is_file() {
        return Err(boxed_err(format!("missing {label}: {}", path.display())));
    }
    Ok(())
}

fn git_head(root: &Path) -> Result<String> {
    util::capture_command(Command::new("git").arg("-C").arg(root).arg("rev-parse").arg("HEAD"))
}

fn ensure_current_checkout_clean(root: &Path) -> Result<()> {
    let changes = util::capture_command(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("status")
            .arg("--porcelain")
            .arg("--untracked-files=normal"),
    )?;
    if !changes.is_empty() {
        return Err(boxed_err("SDK releases must be finalized and uploaded from a clean Git checkout"));
    }
    Ok(())
}

fn validate_archive_paths(tar_program: &OsString, archive: &Path) -> Result<()> {
    let listing = util::capture_command(Command::new(tar_program).arg("-tzf").arg(archive))?;
    if listing.is_empty() {
        return Err(boxed_err(format!("archive is empty: {}", archive.display())));
    }
    for entry in listing.lines() {
        if !archive_entry_is_safe(entry) {
            return Err(boxed_err(format!("archive contains unsafe path '{entry}': {}", archive.display())));
        }
    }
    Ok(())
}

fn archive_entry_is_safe(entry: &str) -> bool {
    let normalized = entry.trim_start_matches("./");
    normalized.is_empty()
        || !Path::new(normalized).components().any(|component| {
            matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))
        })
}

fn archive_member(tar_program: &OsString, archive: &Path, member: &str) -> Result<Vec<u8>> {
    for candidate in [format!("./{member}"), member.to_string()] {
        let output = Command::new(tar_program).arg("-xOzf").arg(archive).arg(&candidate).output()?;
        if output.status.success() {
            return Ok(output.stdout);
        }
    }
    Err(boxed_err(format!("{} is missing {member}", archive.display())))
}

fn archive_manifest(tar_program: &OsString, archive: &Path) -> Result<toml::Value> {
    let manifest_bytes = archive_member(tar_program, archive, "manifest.toml")?;
    let manifest_text = String::from_utf8(manifest_bytes)
        .map_err(|error| boxed_err(format!("{} manifest.toml is not UTF-8: {error}", archive.display())))?;
    toml::from_str(&manifest_text)
        .map_err(|error| boxed_err(format!("parse manifest in {}: {error}", archive.display())))
}

fn validate_common_archive(
    tar_program: &OsString,
    archive: &Path,
    expected_version: &str,
    expected_keyos_version: &str,
    expected_commit: &str,
) -> Result<()> {
    validate_archive_paths(tar_program, archive)?;
    let manifest = archive_manifest(tar_program, archive)?;
    require_manifest_string(&manifest, "sdk", "version", expected_version, archive)?;
    require_manifest_string(&manifest, "sdk", "keyos_version", expected_keyos_version, archive)?;
    require_manifest_string(&manifest, "sdk", "kind", "common", archive)?;
    require_manifest_string(&manifest, "build", "profile", "release", archive)?;
    require_manifest_string(&manifest, "build", "workspace_commit", expected_commit, archive)?;
    require_clean_build_manifest(&manifest, archive)?;
    validate_embedded_docs(tar_program, archive, expected_version, expected_keyos_version)
}

fn validate_target_archive(
    tar_program: &OsString,
    archive: &Path,
    expected_version: &str,
    expected_keyos_version: &str,
    expected_target: &str,
) -> Result<String> {
    validate_archive_paths(tar_program, archive)?;
    let manifest = archive_manifest(tar_program, archive)?;

    require_manifest_string(&manifest, "sdk", "version", expected_version, archive)?;
    require_manifest_string_if_present(&manifest, "sdk", "keyos_version", expected_keyos_version, archive)?;
    require_manifest_string(&manifest, "sdk", "target", expected_target, archive)?;
    require_manifest_string(&manifest, "build", "profile", "release", archive)?;
    require_clean_build_manifest(&manifest, archive)?;
    let workspace_commit = manifest_string(&manifest, "build", "workspace_commit", archive)?.to_string();

    let foundation = archive_member(tar_program, archive, "bin/foundation")?;
    validate_foundation_binary_identity(&foundation, expected_version, &workspace_commit, archive)?;
    validate_binary_architecture(&foundation, expected_target, archive)?;
    Ok(workspace_commit)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmbeddedDocsManifest {
    schema_version: u32,
    sdk_version: String,
    current_keyos_version: String,
    default_keyos_version: String,
    versions: Vec<EmbeddedDocsVersion>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmbeddedDocsVersion {
    keyos_version: String,
    path: String,
}

fn validate_embedded_docs(
    tar_program: &OsString,
    archive: &Path,
    expected_sdk_version: &str,
    expected_keyos_version: &str,
) -> Result<()> {
    for member in ["docs/api/index.html", "docs/api/version-selector.js"] {
        archive_member(tar_program, archive, member)?;
    }
    let manifest_bytes = archive_member(tar_program, archive, "docs/api/bundle-manifest.json")?;
    let script_bytes = archive_member(tar_program, archive, "docs/api/bundle-manifest.js")?;
    let manifest = validate_embedded_docs_manifest(
        &manifest_bytes,
        &script_bytes,
        expected_sdk_version,
        expected_keyos_version,
    )?;
    archive_member(tar_program, archive, &format!("docs/api/{}index.html", manifest.versions[0].path))?;
    Ok(())
}

fn validate_embedded_docs_manifest(
    manifest_bytes: &[u8],
    script_bytes: &[u8],
    expected_sdk_version: &str,
    expected_keyos_version: &str,
) -> Result<EmbeddedDocsManifest> {
    let manifest_text = std::str::from_utf8(manifest_bytes)
        .map_err(|error| boxed_err(format!("embedded docs bundle-manifest.json is not UTF-8: {error}")))?;
    let manifest: EmbeddedDocsManifest = serde_json::from_str(manifest_text)
        .map_err(|error| boxed_err(format!("parse embedded docs bundle-manifest.json: {error}")))?;
    if manifest.schema_version != 1 {
        return Err(boxed_err(format!(
            "embedded docs schemaVersion is {}, expected 1",
            manifest.schema_version
        )));
    }
    if manifest.sdk_version != expected_sdk_version {
        return Err(boxed_err(format!(
            "embedded docs sdkVersion is '{}', expected '{}'",
            manifest.sdk_version, expected_sdk_version
        )));
    }
    for (field, actual) in [
        ("currentKeyosVersion", manifest.current_keyos_version.as_str()),
        ("defaultKeyosVersion", manifest.default_keyos_version.as_str()),
    ] {
        if actual != expected_keyos_version {
            return Err(boxed_err(format!(
                "embedded docs {field} is '{actual}', expected '{expected_keyos_version}'"
            )));
        }
    }
    if manifest.versions.len() != 1 {
        return Err(boxed_err(format!(
            "embedded SDK docs must contain exactly one KeyOS version, found {}",
            manifest.versions.len()
        )));
    }
    let version = &manifest.versions[0];
    if version.keyos_version != expected_keyos_version {
        return Err(boxed_err(format!(
            "embedded docs versions[0].keyosVersion is '{}', expected '{}'",
            version.keyos_version, expected_keyos_version
        )));
    }
    let expected_path = format!("v{expected_keyos_version}/");
    if version.path != expected_path {
        return Err(boxed_err(format!(
            "embedded docs versions[0].path is '{}', expected '{expected_path}'",
            version.path
        )));
    }
    let expected_script = format!("window.KEYOS_DOCS_BUNDLE_MANIFEST = {manifest_text};\n");
    if script_bytes != expected_script.as_bytes() {
        return Err(boxed_err("embedded docs bundle-manifest.js does not match bundle-manifest.json"));
    }
    Ok(manifest)
}

fn require_clean_build_manifest(manifest: &toml::Value, archive: &Path) -> Result<()> {
    let dirty = manifest
        .get("build")
        .and_then(|value| value.get("workspace_dirty"))
        .and_then(toml::Value::as_bool)
        .ok_or_else(|| boxed_err(format!("{} manifest has no build.workspace_dirty", archive.display())))?;
    if dirty {
        return Err(boxed_err(format!("{} was built from a dirty workspace", archive.display())));
    }
    Ok(())
}

fn require_manifest_string(
    manifest: &toml::Value,
    table: &str,
    field: &str,
    expected: &str,
    archive: &Path,
) -> Result<()> {
    let actual = manifest_string(manifest, table, field, archive)?;
    if actual != expected {
        return Err(boxed_err(format!(
            "{} manifest {table}.{field} is '{actual}', expected '{expected}'",
            archive.display()
        )));
    }
    Ok(())
}

fn require_manifest_string_if_present(
    manifest: &toml::Value,
    table: &str,
    field: &str,
    expected: &str,
    archive: &Path,
) -> Result<()> {
    let Some(actual) = manifest.get(table).and_then(|value| value.get(field)) else {
        return Ok(());
    };
    let actual = actual.as_str().ok_or_else(|| {
        boxed_err(format!("{} manifest {table}.{field} is not a string", archive.display()))
    })?;
    if actual != expected {
        return Err(boxed_err(format!(
            "{} manifest {table}.{field} is '{actual}', expected '{expected}'",
            archive.display()
        )));
    }
    Ok(())
}

fn manifest_string<'a>(
    manifest: &'a toml::Value,
    table: &str,
    field: &str,
    archive: &Path,
) -> Result<&'a str> {
    manifest
        .get(table)
        .and_then(|value| value.get(field))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| boxed_err(format!("{} manifest has no {table}.{field}", archive.display())))
}

fn validate_foundation_binary_identity(
    bytes: &[u8],
    expected_sdk_version: &str,
    expected_commit: &str,
    archive: &Path,
) -> Result<()> {
    let short_commit = expected_commit
        .get(..12)
        .ok_or_else(|| boxed_err(format!("workspace commit is too short: {expected_commit}")))?;
    let expected = format!("{expected_sdk_version} ({short_commit})");
    if !bytes.windows(expected.len()).any(|window| window == expected.as_bytes()) {
        return Err(boxed_err(format!(
            "{} foundation binary does not contain expected SDK identity '{expected}'",
            archive.display()
        )));
    }
    Ok(())
}

fn validate_binary_architecture(bytes: &[u8], target: &str, archive: &Path) -> Result<()> {
    if !util::command_exists("file") {
        return Err(boxed_err("release validation requires the 'file' command"));
    }
    let mut temp = tempfile::NamedTempFile::new()?;
    temp.write_all(bytes)?;
    let description = util::capture_command(Command::new("file").arg("-b").arg(temp.path()))?;
    if !binary_description_matches_target(&description, target) {
        return Err(boxed_err(format!(
            "{} contains a foundation binary that does not match {target}: {description}",
            archive.display()
        )));
    }
    if !binary_description_is_portable_for_target(&description, target) {
        return Err(boxed_err(format!(
            "{} contains a dynamically linked ARM Linux foundation binary; ARM Linux releases must be statically linked: {description}",
            archive.display()
        )));
    }
    Ok(())
}

fn binary_description_matches_target(description: &str, target: &str) -> bool {
    let lower = description.to_ascii_lowercase();
    match target {
        value if value.starts_with("aarch64-") && value.ends_with("-apple-darwin") => {
            lower.contains("mach-o") && (lower.contains("arm64") || lower.contains("aarch64"))
        }
        value if value.starts_with("x86_64-") && value.ends_with("-apple-darwin") => {
            lower.contains("mach-o") && lower.contains("x86_64")
        }
        value if value.starts_with("aarch64-") && value.contains("-linux-") => {
            lower.contains("elf") && (lower.contains("aarch64") || lower.contains("arm64"))
        }
        value if value.starts_with("x86_64-") && value.contains("-linux-") => {
            lower.contains("elf") && (lower.contains("x86-64") || lower.contains("x86_64"))
        }
        value if value.starts_with("aarch64-") && value.contains("-windows-") => {
            lower.contains("pe32+") && (lower.contains("aarch64") || lower.contains("arm64"))
        }
        value if value.starts_with("x86_64-") && value.contains("-windows-") => {
            lower.contains("pe32+") && (lower.contains("x86-64") || lower.contains("x86_64"))
        }
        _ => false,
    }
}

fn binary_description_is_portable_for_target(description: &str, target: &str) -> bool {
    if !(target.starts_with("aarch64-") && target.contains("-linux-")) {
        return true;
    }
    let lower = description.to_ascii_lowercase();
    lower.contains("statically linked") || lower.contains("static-pie linked")
}

fn validate_written_metadata(
    release_dir: &Path,
    version: &str,
    targets: &[String],
    base_url: &str,
    written: &WrittenReleaseMetadata,
) -> Result<()> {
    validate_installer(release_dir, version, targets, base_url)?;
    validate_checksums(release_dir, &checksummed_file_names(version, targets))?;
    let gpg = package::find_gpg()?.ok_or_else(|| boxed_err("release verification requires gpg or gpg2"))?;
    let fingerprint = written
        .signing_fingerprint
        .as_deref()
        .ok_or_else(|| boxed_err("finalized release has no signing fingerprint"))?;
    for signature in written.files.iter().filter(|path| path.extension().is_some_and(|ext| ext == "sig")) {
        let signed = PathBuf::from(signature.to_string_lossy().trim_end_matches(".sig"));
        verify_signature(&gpg, signature, &signed, fingerprint)?;
    }
    Ok(())
}

fn validate_installer(release_dir: &Path, version: &str, targets: &[String], base_url: &str) -> Result<()> {
    let install = release_dir.join("install.sh");
    util::run_command(Command::new("sh").arg("-n").arg(&install), false)?;
    let script = fs::read_to_string(&install)?;
    for expected in [
        format!("VERSION=\"{version}\""),
        format!("DEFAULT_BASE_URL=\"{base_url}\""),
        format!("SUPPORTED_TARGETS=\"{}\"", targets.join(" ")),
    ] {
        if !script.contains(&expected) {
            return Err(boxed_err(format!("install.sh is missing expected release value: {expected}")));
        }
    }
    Ok(())
}

fn checksummed_file_names(version: &str, targets: &[String]) -> BTreeSet<String> {
    let mut names = BTreeSet::from([common_archive_name(version), "install.sh".to_string()]);
    names.extend(targets.iter().map(|target| target_archive_name(version, target)));
    names
}

fn finalized_file_names(version: &str, targets: &[String]) -> BTreeSet<String> {
    let mut names = checksummed_file_names(version, targets);
    names.insert("checksums.sha256".to_string());
    let signatures = names.iter().map(|name| format!("{name}.sig")).collect::<Vec<_>>();
    names.extend(signatures);
    names
}

fn validate_checksums(release_dir: &Path, expected_names: &BTreeSet<String>) -> Result<()> {
    let contents = fs::read_to_string(release_dir.join("checksums.sha256"))?;
    let mut seen = BTreeSet::new();
    for (index, line) in contents.lines().enumerate() {
        let (expected, name) = line
            .split_once("  ")
            .ok_or_else(|| boxed_err(format!("invalid checksums.sha256 line {}: {line}", index + 1)))?;
        if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
            return Err(boxed_err(format!("unsafe checksum filename: {name}")));
        }
        if !seen.insert(name.to_string()) {
            return Err(boxed_err(format!("duplicate checksum entry: {name}")));
        }
        let actual = util::sha256(&release_dir.join(name))?;
        if actual != expected {
            return Err(boxed_err(format!("checksum mismatch for {name}")));
        }
    }
    if &seen != expected_names {
        return Err(boxed_err(format!(
            "checksums.sha256 file set mismatch: {}",
            seen.symmetric_difference(expected_names).cloned().collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(())
}

fn verify_signature(
    gpg: &OsString,
    signature: &Path,
    signed: &Path,
    expected_fingerprint: &str,
) -> Result<()> {
    let output = Command::new(gpg)
        .arg("--batch")
        .arg("--status-fd=1")
        .arg("--verify")
        .arg(signature)
        .arg(signed)
        .output()?;
    if !output.status.success() {
        return Err(boxed_err(format!(
            "signature verification failed for {}: {}",
            signed.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let status = String::from_utf8_lossy(&output.stdout);
    let fingerprint_matches = status.lines().any(|line| {
        line.strip_prefix("[GNUPG:] VALIDSIG ").is_some_and(|fields| {
            fields.split_whitespace().any(|field| field.eq_ignore_ascii_case(expected_fingerprint))
        })
    });
    if !fingerprint_matches {
        return Err(boxed_err(format!(
            "signature for {} is not from expected key {expected_fingerprint}",
            signed.display()
        )));
    }
    Ok(())
}

fn smoke_install_native(
    release_dir: &Path,
    expected_sdk_version: &str,
    targets: &[String],
    verbose: bool,
) -> Result<()> {
    let host = host_target()?;
    if !targets.iter().any(|target| target == &host) {
        eprintln!("warning: skipping installer smoke test because {host} is not in this release");
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let install_root = temp.path().join("install");
    let base_url = format!("file://{}", release_dir.display());
    let mut command = Command::new("sh");
    command
        .arg(release_dir.join("install.sh"))
        .env("FOUNDATION_SDK_BASE_URL", base_url)
        .env("FOUNDATION_SDK_INSTALL_DIR", &install_root)
        .env("FOUNDATION_SDK_UPDATE_RC", "0");
    util::run_command(&mut command, verbose)?;
    let foundation = install_root.join("bin/foundation");
    if !foundation.is_file() {
        return Err(boxed_err("installer smoke test did not create bin/foundation"));
    }
    let reported_version = util::capture_command(Command::new(&foundation).arg("--version"))?;
    let expected_prefix = format!("foundation {expected_sdk_version} (");
    if !reported_version.starts_with(&expected_prefix) {
        return Err(boxed_err(format!(
            "installed foundation binary reports '{reported_version}', expected SDK {expected_sdk_version}"
        )));
    }
    // Without it every 'foundation update' from this release refuses to run.
    if !install_root.join("current/share/foundation-sdk-release.asc").is_file() {
        return Err(boxed_err("installer smoke test did not install the release key"));
    }
    Ok(())
}

fn host_target() -> Result<String> {
    let output = util::capture_command(Command::new("rustc").arg("-vV"))?;
    output
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| boxed_err("could not determine host target from rustc -vV"))
}

#[allow(clippy::too_many_arguments)]
fn write_release_manifest(
    release_dir: &Path,
    version: &str,
    keyos_version: &str,
    release: &str,
    base_url: &str,
    targets: &[String],
    workspace_commit: &str,
    target_workspace_commits: &BTreeMap<String, String>,
    written: &WrittenReleaseMetadata,
) -> Result<()> {
    let mut files = Vec::new();
    for path in &written.files {
        let name = util::display_name(path);
        files.push(ReleaseFile {
            kind: release_file_kind(&name).to_string(),
            sha256: util::sha256(path)?,
            size: fs::metadata(path)?.len(),
            name,
        });
    }
    files.sort_by(|left, right| left.name.cmp(&right.name));

    let manifest = ReleaseManifest {
        format_version: 1,
        sdk_version: version.to_string(),
        keyos_version: keyos_version.to_string(),
        release: release.to_string(),
        base_url: base_url.to_string(),
        targets: targets.to_vec(),
        workspace_commit: workspace_commit.to_string(),
        target_workspace_commits: target_workspace_commits.clone(),
        signing_fingerprint: written
            .signing_fingerprint
            .clone()
            .ok_or_else(|| boxed_err("finalized release has no signing fingerprint"))?,
        files,
    };
    fs::write(release_dir.join(RELEASE_MANIFEST_NAME), toml::to_string_pretty(&manifest)?)?;
    Ok(())
}

fn release_file_kind(name: &str) -> &'static str {
    if name == "install.sh" {
        "installer"
    } else if name == "checksums.sha256" {
        "checksums"
    } else if name.ends_with(".tar.gz") {
        "archive"
    } else if name.ends_with(".sig") {
        "signature"
    } else {
        "other"
    }
}

fn load_release_manifest(release_dir: &Path) -> Result<ReleaseManifest> {
    let path = release_dir.join(RELEASE_MANIFEST_NAME);
    let contents =
        fs::read_to_string(&path).map_err(|error| boxed_err(format!("read {}: {error}", path.display())))?;
    toml::from_str(&contents).map_err(|error| boxed_err(format!("parse {}: {error}", path.display())))
}

fn validate_release_directory(
    release_dir: &Path,
    version: &str,
    keyos_version: &str,
    release: &str,
) -> Result<ReleaseManifest> {
    let manifest = load_release_manifest(release_dir)?;
    if manifest.format_version != 1 {
        return Err(boxed_err(format!("unsupported release manifest format {}", manifest.format_version)));
    }
    if manifest.sdk_version != version || manifest.release != release {
        return Err(boxed_err(format!("release manifest identity mismatch: expected {release} / {version}")));
    }
    if manifest.keyos_version != keyos_version {
        return Err(boxed_err(format!(
            "release manifest KeyOS version is '{}', expected '{}'",
            manifest.keyos_version, keyos_version
        )));
    }
    if manifest.base_url != format!("{PUBLIC_DOWNLOAD_ROOT}/{release}") {
        return Err(boxed_err("release manifest has an unexpected base_url"));
    }
    if manifest.signing_fingerprint.is_empty()
        || !fs::read_to_string(release_dir.join("install.sh"))?
            .contains(&format!("EMBEDDED_GPG_PUBLIC_KEY_FINGERPRINT=\"{}\"", manifest.signing_fingerprint))
    {
        return Err(boxed_err("release signing fingerprint does not match install.sh"));
    }

    let configured_targets = manifest.targets.iter().cloned().collect::<BTreeSet<_>>();
    if configured_targets.len() != manifest.targets.len() || configured_targets.is_empty() {
        return Err(boxed_err("release manifest targets must be non-empty and unique"));
    }
    let committed_targets = manifest.target_workspace_commits.keys().cloned().collect::<BTreeSet<_>>();
    if committed_targets != configured_targets {
        return Err(boxed_err("release manifest target commit set does not match its targets"));
    }

    let expected_manifest_files = finalized_file_names(version, &manifest.targets);
    let mut manifest_files = BTreeSet::new();
    for file in &manifest.files {
        if file.name.contains('/') || file.name.contains('\\') || file.name == "." || file.name == ".." {
            return Err(boxed_err(format!("unsafe release filename: {}", file.name)));
        }
        if !manifest_files.insert(file.name.clone()) {
            return Err(boxed_err(format!("duplicate release file: {}", file.name)));
        }
        if file.kind != release_file_kind(&file.name) {
            return Err(boxed_err(format!("release file has incorrect kind: {}", file.name)));
        }
        let path = release_dir.join(&file.name);
        require_file(&path, "release file")?;
        if fs::metadata(&path)?.len() != file.size || util::sha256(&path)? != file.sha256 {
            return Err(boxed_err(format!("release file does not match manifest: {}", file.name)));
        }
    }
    if manifest_files != expected_manifest_files {
        return Err(boxed_err(format!(
            "release.toml file set mismatch: {}",
            manifest_files
                .symmetric_difference(&expected_manifest_files)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    let mut expected_files = expected_manifest_files;
    expected_files.insert(RELEASE_MANIFEST_NAME.to_string());
    let actual_files = fs::read_dir(release_dir)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().to_string()))
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    if actual_files != expected_files {
        return Err(boxed_err(format!(
            "release directory contains files not declared by release.toml: {}",
            actual_files.symmetric_difference(&expected_files).cloned().collect::<Vec<_>>().join(", ")
        )));
    }
    validate_installer(release_dir, version, &manifest.targets, &manifest.base_url)?;
    validate_checksums(release_dir, &checksummed_file_names(version, &manifest.targets))?;
    Ok(manifest)
}

pub fn upload(root: &Path, config: &Config, args: &UploadArgs) -> Result<()> {
    ensure_current_checkout_clean(root)?;
    let version_text = validated_sdk_version(root, config, None)?;
    let keyos_version = validated_keyos_version(config, Some(&config.sdk.keyos_version))?;
    let requested_version = version_from_release_label(&args.release)?;
    let configured_version = Version::parse(&version_text)?;
    if requested_version != configured_version {
        return Err(boxed_err(format!(
            "release {} represents SDK {}, but sdk-build.toml is {}",
            args.release, requested_version, configured_version
        )));
    }

    let output_dir = util::absolute_path(root, &args.output_dir);
    let release_dir = output_dir.join("releases").join(&args.release);
    let manifest = validate_release_directory(&release_dir, &version_text, &keyos_version, &args.release)?;
    let configured_targets = selected_targets(config, &manifest.targets)?;
    if configured_targets != manifest.targets {
        return Err(boxed_err("release targets are not in canonical sdk-build.toml order"));
    }
    let tar_program = package::find_gnu_tar()?.ok_or_else(|| boxed_err("GNU tar is required"))?;
    validate_common_archive(
        &tar_program,
        &release_dir.join(common_archive_name(&version_text)),
        &version_text,
        &keyos_version,
        &manifest.workspace_commit,
    )?;
    for target in &manifest.targets {
        let target_commit = manifest
            .target_workspace_commits
            .get(target)
            .ok_or_else(|| boxed_err(format!("release manifest has no workspace commit for {target}")))?;
        let actual_commit = validate_target_archive(
            &tar_program,
            &release_dir.join(target_archive_name(&version_text, target)),
            &version_text,
            &keyos_version,
            target,
        )?;
        if &actual_commit != target_commit {
            return Err(boxed_err(format!(
                "{target} archive workspace commit is {actual_commit}, expected {target_commit} from release.toml"
            )));
        }
    }
    validate_uploaded_signatures(&release_dir, &manifest)?;

    if !args.dry_run && !util::command_exists("gcloud") {
        return Err(boxed_err("gcloud is required to upload Foundation SDK releases"));
    }

    let manifest_path = release_dir.join(RELEASE_MANIFEST_NAME);
    let manifest_file = ReleaseFile {
        name: RELEASE_MANIFEST_NAME.to_string(),
        sha256: util::sha256(&manifest_path)?,
        size: fs::metadata(&manifest_path)?.len(),
        kind: "manifest".to_string(),
    };
    let destination = format!("{}/{}", args.bucket, args.release);
    let staging = temporary_release_destination(&args.bucket, &args.release)?;
    let files = ordered_upload_files(&manifest, &manifest_file)?;
    let live_generations = if args.dry_run {
        files.iter().map(|file| (file.name.clone(), "0".to_string())).collect::<BTreeMap<_, _>>()
    } else {
        ensure_bucket_accessible(&args.bucket)?;
        let generations = snapshot_destination_generations(&destination, &files)?;
        let existing = list_release_objects(&destination)?;
        if !existing.is_empty() {
            confirm_release_replacement(&destination, existing.len())?;
        }
        generations
    };

    let upload_result = (|| {
        upload_local_release(&release_dir, &staging, &files, args)?;
        promote_staged_release(&staging, &destination, &files, &live_generations, args)?;
        remove_stale_release_objects(&destination, &files, args)?;
        if args.link_as_latest {
            promote_latest(&destination, &manifest, args)?;
        }
        Ok(())
    })();
    let cleanup_result = cleanup_staged_release(&staging, args);
    match (upload_result, cleanup_result) {
        (Err(upload_error), Err(cleanup_error)) => {
            return Err(boxed_err(format!(
                "{upload_error}; additionally failed to clean temporary upload {staging}: {cleanup_error}"
            )));
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
        (Ok(()), Ok(())) => {}
    }

    if args.dry_run {
        println!("validated SDK {} for upload to {destination}; no cloud changes made", manifest.sdk_version);
    } else {
        println!("uploaded SDK {} to {destination}", manifest.sdk_version);
    }
    Ok(())
}

fn validate_uploaded_signatures(release_dir: &Path, manifest: &ReleaseManifest) -> Result<()> {
    let gpg = package::find_gpg()?.ok_or_else(|| boxed_err("release verification requires gpg or gpg2"))?;
    let names = manifest.files.iter().map(|file| file.name.as_str()).collect::<BTreeSet<_>>();
    for file in manifest.files.iter().filter(|file| file.kind != "signature") {
        let signature_name = format!("{}.sig", file.name);
        if !names.contains(signature_name.as_str()) {
            return Err(boxed_err(format!("release is missing signature: {signature_name}")));
        }
        verify_signature(
            &gpg,
            &release_dir.join(&signature_name),
            &release_dir.join(&file.name),
            &manifest.signing_fingerprint,
        )?;
    }
    Ok(())
}

fn ensure_bucket_accessible(bucket: &str) -> Result<()> {
    let output = Command::new("gcloud").arg("storage").arg("ls").arg(bucket).output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(boxed_err(format!(
        "cannot access upload bucket {bucket}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn list_release_objects(destination: &str) -> Result<Vec<String>> {
    let output = Command::new("gcloud").arg("storage").arg("ls").arg(format!("{destination}/**")).output()?;
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stderr).contains("matched no objects") {
            return Ok(Vec::new());
        }
        return Err(boxed_err(format!(
            "could not check release prefix {destination}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let prefix = format!("{destination}/");
    let objects = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .collect::<Vec<_>>();
    for object in &objects {
        object
            .strip_prefix(&prefix)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| boxed_err(format!("object is outside release prefix {destination}: {object}")))?;
    }
    Ok(objects)
}

fn confirm_release_replacement(destination: &str, object_count: usize) -> Result<()> {
    eprint!(
        "release destination {destination} already contains {object_count} object(s). Stage and promote a complete replacement? [y/N] "
    );
    io::stderr().flush()?;
    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    if matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    Err(boxed_err("upload cancelled; the existing release was not changed"))
}

fn delete_release_objects(destination: &str, objects: &[String], args: &UploadArgs) -> Result<()> {
    delete_objects(objects, args)?;

    let remaining = list_release_objects(destination)?;
    if !remaining.is_empty() {
        return Err(boxed_err(format!(
            "temporary upload destination {destination} is not empty after cleanup"
        )));
    }
    Ok(())
}

fn delete_objects(objects: &[String], args: &UploadArgs) -> Result<()> {
    if objects.is_empty() {
        return Ok(());
    }
    let mut command = Command::new("gcloud");
    command.arg("storage").arg("rm").arg("--quiet").args(objects);
    util::run_command(&mut command, args.verbose)?;
    Ok(())
}

fn stale_release_objects(destination: &str, objects: &[String], files: &[&ReleaseFile]) -> Vec<String> {
    let expected = files.iter().map(|file| format!("{destination}/{}", file.name)).collect::<BTreeSet<_>>();
    objects.iter().filter(|object| !expected.contains(*object)).cloned().collect()
}

fn remove_stale_release_objects(destination: &str, files: &[&ReleaseFile], args: &UploadArgs) -> Result<()> {
    if args.dry_run {
        return Ok(());
    }
    let objects = list_release_objects(destination)?;
    let stale = stale_release_objects(destination, &objects, files);
    delete_objects(&stale, args)?;

    let remaining = list_release_objects(destination)?;
    let stale = stale_release_objects(destination, &remaining, files);
    if !stale.is_empty() {
        return Err(boxed_err(format!(
            "release destination {destination} still contains objects omitted from the replacement: {}",
            stale.join(", ")
        )));
    }
    Ok(())
}

fn temporary_release_destination(bucket: &str, release: &str) -> Result<String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| boxed_err(format!("system clock is before Unix epoch: {error}")))?
        .as_nanos();
    Ok(staging_destination(bucket, release, &format!("{}-{timestamp}", std::process::id())))
}

fn staging_destination(bucket: &str, release: &str, unique: &str) -> String {
    format!("{bucket}/.staging/{release}-{unique}")
}

fn ordered_upload_files<'a>(
    manifest: &'a ReleaseManifest,
    manifest_file: &'a ReleaseFile,
) -> Result<Vec<&'a ReleaseFile>> {
    let installer = manifest
        .files
        .iter()
        .find(|file| file.name == "install.sh")
        .ok_or_else(|| boxed_err("release is missing install.sh"))?;
    let mut files = manifest.files.iter().filter(|file| file.name != "install.sh").collect::<Vec<_>>();
    files.sort_by_key(|file| match file.name.as_str() {
        "install.sh.sig" => 3,
        "checksums.sha256.sig" => 2,
        "checksums.sha256" => 1,
        _ => 0,
    });
    files.push(manifest_file);
    files.push(installer);
    Ok(files)
}

fn snapshot_destination_generations(
    destination: &str,
    files: &[&ReleaseFile],
) -> Result<BTreeMap<String, String>> {
    files
        .iter()
        .map(|file| {
            let object = format!("{destination}/{}", file.name);
            destination_generation(&object).map(|generation| (file.name.clone(), generation))
        })
        .collect()
}

fn upload_local_release(
    release_dir: &Path,
    staging: &str,
    files: &[&ReleaseFile],
    args: &UploadArgs,
) -> Result<()> {
    for file in files {
        let source = release_dir.join(&file.name);
        let destination = format!("{staging}/{}", file.name);
        run_gcloud_copy(&source, &destination, file, "public,no-cache,max-age=0", Some("0"), args)?;
    }
    Ok(())
}

fn promote_staged_release(
    staging: &str,
    destination: &str,
    files: &[&ReleaseFile],
    generations: &BTreeMap<String, String>,
    args: &UploadArgs,
) -> Result<()> {
    for file in files {
        let source = PathBuf::from(format!("{staging}/{}", file.name));
        let destination = format!("{destination}/{}", file.name);
        let generation = generations
            .get(&file.name)
            .ok_or_else(|| boxed_err(format!("missing destination generation for {}", file.name)))?;
        run_gcloud_copy(&source, &destination, file, "public,no-cache,max-age=0", Some(generation), args)?;
    }
    Ok(())
}

fn cleanup_staged_release(staging: &str, args: &UploadArgs) -> Result<()> {
    if args.dry_run {
        return Ok(());
    }
    let objects = list_release_objects(staging)?;
    if objects.is_empty() {
        return Ok(());
    }
    delete_release_objects(staging, &objects, args)
}

fn run_gcloud_copy(
    source: &Path,
    destination: &str,
    file: &ReleaseFile,
    cache_control: &str,
    generation: Option<&str>,
    args: &UploadArgs,
) -> Result<()> {
    if !args.dry_run && generation.is_none() {
        return Err(boxed_err(format!("upload generation precondition missing for {destination}")));
    }
    let mut command = Command::new("gcloud");
    command.arg("storage").arg("cp");
    command
        .arg(format!("--cache-control={cache_control}"))
        .arg(format!("--content-type={}", content_type(&file.name)))
        .arg(format!("--custom-metadata=sha256={},release={}", file.sha256, args.release));
    if let Some(generation) = generation {
        command.arg(format!("--if-generation-match={generation}"));
    }
    command.arg(source).arg(destination);
    run_or_print(&mut command, args)?;
    if !args.dry_run {
        verify_remote_object(destination, file, &args.release)?;
    }
    Ok(())
}

fn verify_remote_object(destination: &str, file: &ReleaseFile, expected_release: &str) -> Result<()> {
    let output = util::capture_command(
        Command::new("gcloud")
            .arg("storage")
            .arg("objects")
            .arg("describe")
            .arg(destination)
            .arg("--format=json"),
    )?;
    let object: serde_json::Value = serde_json::from_str(&output)?;
    let size = object
        .get("size")
        .and_then(|value| value.as_u64().or_else(|| value.as_str().and_then(|value| value.parse().ok())))
        .ok_or_else(|| boxed_err(format!("uploaded object has no size: {destination}")))?;
    let sha256 = remote_custom_metadata(&object, "sha256")
        .ok_or_else(|| boxed_err(format!("uploaded object has no sha256 metadata: {destination}")))?;
    let release = remote_custom_metadata(&object, "release")
        .ok_or_else(|| boxed_err(format!("uploaded object has no release metadata: {destination}")))?;
    if size != file.size || sha256 != file.sha256 || release != expected_release {
        return Err(boxed_err(format!("uploaded object failed verification: {destination}")));
    }
    Ok(())
}

fn remote_custom_metadata<'a>(object: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    ["custom_fields", "metadata"].into_iter().find_map(|field| {
        object.get(field).and_then(|value| value.get(name)).and_then(serde_json::Value::as_str)
    })
}

fn content_type(name: &str) -> &'static str {
    if name.ends_with(".tar.gz") {
        "application/gzip"
    } else if name.ends_with(".sig") {
        "application/pgp-signature"
    } else if name.ends_with(".sh") {
        "application/x-sh"
    } else {
        "text/plain; charset=utf-8"
    }
}

fn promote_latest(versioned_destination: &str, manifest: &ReleaseManifest, args: &UploadArgs) -> Result<()> {
    let files = ["install.sh.sig", "install.sh"];
    let by_name = manifest.files.iter().map(|file| (file.name.as_str(), file)).collect::<BTreeMap<_, _>>();
    for name in files {
        let file = by_name.get(name).ok_or_else(|| boxed_err(format!("release is missing {name}")))?;
        let destination = format!("{}/latest/{name}", args.bucket);
        let generation = if args.dry_run { "0".to_string() } else { destination_generation(&destination)? };
        let source = PathBuf::from(format!("{versioned_destination}/{name}"));
        run_gcloud_copy(&source, &destination, file, "no-cache, max-age=0", Some(&generation), args)?;
    }
    Ok(())
}

fn destination_generation(destination: &str) -> Result<String> {
    let output = Command::new("gcloud")
        .arg("storage")
        .arg("objects")
        .arg("describe")
        .arg(destination)
        .arg("--format=value(generation)")
        .output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("not found") || stderr.contains("matched no objects") || stderr.contains("404") {
        return Ok("0".to_string());
    }
    Err(boxed_err(format!("could not inspect {destination}: {}", stderr.trim())))
}

fn run_or_print(command: &mut Command, args: &UploadArgs) -> Result<()> {
    if args.dry_run {
        println!("dry-run: {command:?}");
        return Ok(());
    }
    util::run_command(command, args.verbose)
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    iter.next().ok_or_else(|| boxed_err(format!("missing value for {flag}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use semver::Version;

    use super::{
        archive_entry_is_safe, binary_description_is_portable_for_target, binary_description_matches_target,
        canonical_release_label, ensure_current_checkout_clean, ordered_upload_files, remote_custom_metadata,
        replace_finalized_release, require_manifest_string_if_present, staging_destination,
        stale_release_objects, validate_embedded_docs_manifest, validate_foundation_binary_identity,
        validated_keyos_version, validated_sdk_version, version_from_release_label, FinalizeArgs,
        ReleaseFile, ReleaseManifest, UploadArgs,
    };
    use crate::config::Config;

    #[test]
    fn finalize_accepts_positional_selectors_and_flags() {
        let args = FinalizeArgs::parse(vec![
            "mac-all".into(),
            "linux-x86".into(),
            "--keyos-version".into(),
            "1.4.0-beta3".into(),
            "--sign-key".into(),
            "release@example.com".into(),
            "--verbose".into(),
        ])
        .unwrap();
        assert_eq!(args.targets, ["mac-all", "linux-x86"]);
        assert_eq!(args.keyos_version.as_deref(), Some("1.4.0-beta3"));
        assert_eq!(args.sign_key.as_deref(), Some("release@example.com"));
        assert!(args.verbose);
    }

    #[test]
    fn finalization_can_replace_an_existing_local_release() {
        let temp = tempfile::tempdir().unwrap();
        let release = temp.path().join("v1.0.0");
        let staging = temp.path().join(".v1.0.0.tmp");
        fs::create_dir(&release).unwrap();
        fs::write(release.join("contents"), "old").unwrap();
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("contents"), "new").unwrap();

        assert!(replace_finalized_release(&staging, &release).unwrap());
        assert_eq!(fs::read_to_string(release.join("contents")).unwrap(), "new");
        assert!(!staging.exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn upload_accepts_requested_interface() {
        let args =
            UploadArgs::parse(vec!["v1.1.0".into(), "--link-as-latest".into(), "--dry-run".into()]).unwrap();
        assert_eq!(args.release, "v1.1.0");
        assert_eq!(args.output_dir, PathBuf::from("dist"));
        assert!(args.link_as_latest);
        assert!(args.dry_run);
    }

    #[test]
    fn clean_checkout_check_rejects_untracked_files() {
        let temp = tempfile::tempdir().unwrap();
        assert!(Command::new("git").arg("init").arg("--quiet").arg(temp.path()).status().unwrap().success());
        assert!(ensure_current_checkout_clean(temp.path()).is_ok());

        fs::write(temp.path().join("untracked.txt"), "release input").unwrap();
        assert!(ensure_current_checkout_clean(temp.path())
            .unwrap_err()
            .to_string()
            .contains("clean Git checkout"));
    }

    #[test]
    fn replacement_upload_uses_isolated_staging_prefix_and_installer_last() {
        assert_eq!(
            staging_destination("gs://foundation-sdk", "v1.2.3", "123-456"),
            "gs://foundation-sdk/.staging/v1.2.3-123-456"
        );

        let files = vec![
            ReleaseFile {
                name: "install.sh".into(),
                sha256: "installer".into(),
                size: 1,
                kind: "installer".into(),
            },
            ReleaseFile {
                name: "checksums.sha256".into(),
                sha256: "checksums".into(),
                size: 1,
                kind: "checksums".into(),
            },
            ReleaseFile {
                name: "foundation-sdk-1.2.3-common.tar.gz".into(),
                sha256: "archive".into(),
                size: 1,
                kind: "archive".into(),
            },
            ReleaseFile {
                name: "install.sh.sig".into(),
                sha256: "installer-signature".into(),
                size: 1,
                kind: "signature".into(),
            },
        ];
        let manifest = ReleaseManifest {
            format_version: 1,
            sdk_version: "1.2.3".into(),
            keyos_version: "1.4.0-beta3".into(),
            release: "v1.2.3".into(),
            base_url: "https://sdk.foundation.xyz/v1.2.3".into(),
            targets: vec!["aarch64-apple-darwin".into()],
            workspace_commit: "abc".into(),
            target_workspace_commits: BTreeMap::from([(
                "aarch64-apple-darwin".into(),
                "target-commit".into(),
            )]),
            signing_fingerprint: "fingerprint".into(),
            files,
        };
        let manifest_file = ReleaseFile {
            name: "release.toml".into(),
            sha256: "manifest".into(),
            size: 1,
            kind: "manifest".into(),
        };

        let order = ordered_upload_files(&manifest, &manifest_file)
            .unwrap()
            .into_iter()
            .map(|file| file.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            [
                "foundation-sdk-1.2.3-common.tar.gz",
                "checksums.sha256",
                "install.sh.sig",
                "release.toml",
                "install.sh"
            ]
        );
    }

    #[test]
    fn replacement_upload_identifies_objects_omitted_from_the_new_release() {
        let files = [
            ReleaseFile {
                name: "release.toml".into(),
                sha256: "manifest".into(),
                size: 1,
                kind: "manifest".into(),
            },
            ReleaseFile {
                name: "install.sh".into(),
                sha256: "installer".into(),
                size: 1,
                kind: "installer".into(),
            },
        ];
        let file_refs = files.iter().collect::<Vec<_>>();
        let objects = [
            "gs://foundation-sdk/v1.2.3/release.toml".to_string(),
            "gs://foundation-sdk/v1.2.3/install.sh".to_string(),
            "gs://foundation-sdk/v1.2.3/foundation-sdk-1.2.3-retired-target.tar.gz".to_string(),
        ];

        assert_eq!(
            stale_release_objects("gs://foundation-sdk/v1.2.3", &objects, &file_refs),
            ["gs://foundation-sdk/v1.2.3/foundation-sdk-1.2.3-retired-target.tar.gz"]
        );
    }

    #[test]
    fn release_labels_require_full_semver() {
        assert_eq!(canonical_release_label(&Version::parse("1.1.0").unwrap()).unwrap(), "v1.1.0");
        assert_eq!(canonical_release_label(&Version::parse("1.1.2").unwrap()).unwrap(), "v1.1.2");
        assert_eq!(version_from_release_label("v1.1.0").unwrap(), Version::parse("1.1.0").unwrap());
        assert!(version_from_release_label("v1.1").is_err());
        assert!(version_from_release_label("1.1.0").is_err());
    }

    #[test]
    fn remote_metadata_supports_gcloud_and_rest_json_fields() {
        let gcloud = serde_json::json!({"custom_fields": {"sha256": "abc", "release": "v1.0.0"}});
        assert_eq!(remote_custom_metadata(&gcloud, "sha256"), Some("abc"));
        assert_eq!(remote_custom_metadata(&gcloud, "release"), Some("v1.0.0"));

        let rest = serde_json::json!({"metadata": {"sha256": "def", "release": "v1.0.1"}});
        assert_eq!(remote_custom_metadata(&rest, "sha256"), Some("def"));
        assert_eq!(remote_custom_metadata(&rest, "release"), Some("v1.0.1"));
    }

    #[test]
    fn sdk_version_must_match_both_cargo_workspaces() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("crates/cli")).unwrap();
        fs::write(temp.path().join("Cargo.toml"), "[workspace.package]\nversion = \"1.1.0\"\n").unwrap();
        fs::write(temp.path().join("crates/cli/Cargo.toml"), "[workspace.package]\nversion = \"1.1.0\"\n")
            .unwrap();
        let mut config = Config::default();
        config.sdk.version = "1.1.0".to_string();

        assert_eq!(validated_sdk_version(temp.path(), &config, Some("1.1.0")).unwrap(), "1.1.0");
        config.sdk.version = "1.2.0".to_string();
        assert!(validated_sdk_version(temp.path(), &config, None)
            .unwrap_err()
            .to_string()
            .contains("does not match"));
    }

    #[test]
    fn keyos_version_is_required_and_must_match_the_sdk_configuration() {
        let mut config = Config::default();
        config.sdk.keyos_version = "1.4.0-beta3".to_string();

        assert_eq!(validated_keyos_version(&config, Some("1.4.0-beta3")).unwrap(), "1.4.0-beta3");
        assert!(validated_keyos_version(&config, None).unwrap_err().to_string().contains("--keyos-version"));
        assert!(validated_keyos_version(&config, Some("1.4.0-beta.3"))
            .unwrap_err()
            .to_string()
            .contains("exactly two periods"));
        assert!(validated_keyos_version(&config, Some("1.4.0"))
            .unwrap_err()
            .to_string()
            .contains("does not match sdk-build.toml"));
    }

    #[test]
    fn embedded_docs_versions_and_browser_manifest_must_match() {
        let json = serde_json::to_string_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "sdkVersion": "1.0.0",
            "currentKeyosVersion": "1.4.0-beta3",
            "defaultKeyosVersion": "1.4.0-beta3",
            "versions": [{
                "keyosVersion": "1.4.0-beta3",
                "path": "v1.4.0-beta3/"
            }]
        }))
        .unwrap();
        let script = format!("window.KEYOS_DOCS_BUNDLE_MANIFEST = {json};\n");

        validate_embedded_docs_manifest(json.as_bytes(), script.as_bytes(), "1.0.0", "1.4.0-beta3").unwrap();
        assert!(validate_embedded_docs_manifest(json.as_bytes(), script.as_bytes(), "1.0.0", "1.4.0-beta2")
            .unwrap_err()
            .to_string()
            .contains("currentKeyosVersion"));
        assert!(validate_embedded_docs_manifest(
            json.as_bytes(),
            b"window.KEYOS_DOCS_BUNDLE_MANIFEST = {};\n",
            "1.0.0",
            "1.4.0-beta3"
        )
        .unwrap_err()
        .to_string()
        .contains("does not match"));
    }

    #[test]
    fn binary_architecture_detection_covers_release_targets() {
        assert!(binary_description_matches_target("Mach-O 64-bit executable arm64", "aarch64-apple-darwin"));
        assert!(binary_description_matches_target("Mach-O 64-bit executable x86_64", "x86_64-apple-darwin"));
        assert!(binary_description_matches_target(
            "ELF 64-bit LSB pie executable, x86-64",
            "x86_64-unknown-linux-gnu"
        ));
        assert!(binary_description_matches_target(
            "PE32+ executable (console) Aarch64, for MS Windows",
            "aarch64-pc-windows-msvc"
        ));
        assert!(!binary_description_matches_target(
            "ELF 64-bit LSB pie executable, x86-64",
            "aarch64-unknown-linux-gnu"
        ));
    }

    #[test]
    fn foundation_binary_identity_contains_sdk_version_and_source_commit() {
        let archive = PathBuf::from("foundation-sdk.tar.gz");
        let bytes = b"binary data 1.0.0 (039881500da0) more binary data";

        validate_foundation_binary_identity(
            bytes,
            "1.0.0",
            "039881500da0d1d14dc3468e7e5852d986bb898b",
            &archive,
        )
        .unwrap();
        assert!(validate_foundation_binary_identity(
            bytes,
            "1.0.1",
            "039881500da0d1d14dc3468e7e5852d986bb898b",
            &archive,
        )
        .unwrap_err()
        .to_string()
        .contains("expected SDK identity"));
    }

    #[test]
    fn reused_target_archives_may_predate_keyos_version_metadata() {
        let archive = PathBuf::from("foundation-sdk.tar.gz");
        let without_keyos: toml::Value = toml::from_str("[sdk]\nversion = \"1.0.0\"\n").unwrap();
        require_manifest_string_if_present(&without_keyos, "sdk", "keyos_version", "1.4.0-beta3", &archive)
            .unwrap();

        let wrong_keyos: toml::Value = toml::from_str("[sdk]\nkeyos_version = \"1.4.0-beta2\"\n").unwrap();
        assert!(require_manifest_string_if_present(
            &wrong_keyos,
            "sdk",
            "keyos_version",
            "1.4.0-beta3",
            &archive,
        )
        .unwrap_err()
        .to_string()
        .contains("expected '1.4.0-beta3'"));
    }

    #[test]
    fn arm_linux_release_binary_must_be_static() {
        assert!(binary_description_is_portable_for_target(
            "ELF 64-bit LSB pie executable, ARM aarch64, static-pie linked",
            "aarch64-unknown-linux-gnu"
        ));
        assert!(!binary_description_is_portable_for_target(
            "ELF 64-bit LSB pie executable, ARM aarch64, dynamically linked",
            "aarch64-unknown-linux-gnu"
        ));
        assert!(binary_description_is_portable_for_target(
            "ELF 64-bit LSB pie executable, x86-64, dynamically linked",
            "x86_64-unknown-linux-gnu"
        ));
    }

    #[test]
    fn archive_paths_cannot_escape_the_install_root() {
        assert!(archive_entry_is_safe("./bin/foundation"));
        assert!(archive_entry_is_safe("ui/ui/theme.slint"));
        assert!(!archive_entry_is_safe("../outside"));
        assert!(!archive_entry_is_safe("./bin/../../outside"));
        assert!(!archive_entry_is_safe("/absolute/path"));
    }
}
