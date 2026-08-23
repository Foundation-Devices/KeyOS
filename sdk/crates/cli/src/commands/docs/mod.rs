// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Open the static API documentation bundled with an installed Foundation SDK.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Args;
use semver::Version;
use serde::Deserialize;

const SDK_HOST_TARGETS: &[&str] =
    &["aarch64-apple-darwin", "x86_64-apple-darwin", "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"];
const DOCS_ROOT_ENV: &str = "FOUNDATION_DOCS_ROOT";
const DEVELOPMENT_LABEL: &str = "development";

#[derive(Args)]
pub struct DocsArgs {
    /// Installed SDK version to open (defaults to current)
    #[arg(value_name = "SDK_VERSION")]
    pub version: Option<Version>,

    /// Print the documentation file URL without opening it
    #[arg(long)]
    pub url: bool,
}

#[derive(Clone, Debug)]
struct DocsSelection {
    label: String,
    docs_root: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocsBundleManifest {
    schema_version: u32,
    default_keyos_version: String,
    versions: Vec<DocsBundleVersion>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocsBundleVersion {
    keyos_version: String,
    path: String,
}

pub fn execute(args: &DocsArgs) -> Result<()> {
    let selection = resolve_selection(args.version.as_ref())?;
    let index = selection.docs_root.join("index.html");
    let url = file_url(&index)?;

    if args.url {
        println!("{url}");
        return Ok(());
    }

    open_browser(&url)
        .with_context(|| format!("Could not open the documentation at {url} in the default web browser"))?;
    println!("Opened Foundation SDK API documentation for {}.", selection.label);
    Ok(())
}

fn resolve_selection(version: Option<&Version>) -> Result<DocsSelection> {
    let install_root = sdk_install_root()?;
    let (label, root) = match version {
        Some(version) => (version.to_string(), resolve_installed_version(&install_root, version)?),
        None => {
            let current = install_root.join("current").join("docs").join("api");
            resolve_default_docs_root(
                current,
                std::env::var_os(DOCS_ROOT_ENV).map(PathBuf::from),
                std::env::var_os("FOUNDATION_SDK_ROOT").map(PathBuf::from),
            )
        }
    };

    validate_docs_bundle(&root, version)?;
    let docs_root = fs::canonicalize(&root)
        .with_context(|| format!("Could not resolve the SDK docs directory {}", root.display()))?;
    Ok(DocsSelection { label, docs_root })
}

fn resolve_default_docs_root(
    installed_current: PathBuf,
    development_docs_root: Option<PathBuf>,
    sdk_root: Option<PathBuf>,
) -> (String, PathBuf) {
    if let Some(root) = development_docs_root {
        (DEVELOPMENT_LABEL.to_string(), root)
    } else if let Some(root) = sdk_root {
        ("current".to_string(), root.join("docs").join("api"))
    } else {
        ("current".to_string(), installed_current)
    }
}

fn resolve_installed_version(install_root: &Path, version: &Version) -> Result<PathBuf> {
    let expected = install_root.join(format!("foundation-sdk-{version}-{}", host_target()?));
    if expected.is_dir() {
        return Ok(expected.join("docs").join("api"));
    }

    let mut matches = fs::read_dir(install_root)
        .with_context(|| format!("Could not read the Foundation SDK directory {}", install_root.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter(|entry| bundle_version(&entry.file_name().to_string_lossy()).as_ref() == Some(version))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    matches.sort();

    match matches.as_slice() {
        [] => bail!(
            "Foundation SDK {version} is not installed under {}. Installed SDKs: {}",
            install_root.display(),
            installed_sdk_names(install_root)
        ),
        [path] => Ok(path.join("docs").join("api")),
        _ => bail!(
            "Multiple Foundation SDK {version} bundles are installed, but none matches this host ({}): {}",
            host_target()?,
            matches.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn validate_docs_bundle(root: &Path, version: Option<&Version>) -> Result<()> {
    if !root.is_dir() {
        let selection =
            version.map(|version| format!("SDK {version}")).unwrap_or_else(|| "current SDK".to_string());
        bail!(
            "API documentation for the {selection} was not found at {}. Reinstall an SDK package that includes docs/api.",
            root.display()
        );
    }

    for required in ["index.html", "bundle-manifest.json", "bundle-manifest.js", "version-selector.js"] {
        if !root.join(required).is_file() {
            bail!(
                "The API documentation for the selected SDK is incomplete: {} is missing. Reinstall an SDK package containing the complete versioned docs bundle.",
                root.join(required).display()
            );
        }
    }

    let manifest_path = root.join("bundle-manifest.json");
    let manifest: DocsBundleManifest = serde_json::from_reader(
        File::open(&manifest_path)
            .with_context(|| format!("Could not open docs manifest {}", manifest_path.display()))?,
    )
    .with_context(|| format!("Could not parse docs manifest {}", manifest_path.display()))?;
    if manifest.schema_version != 1 || manifest.versions.is_empty() {
        bail!("The SDK docs manifest has an unsupported schema or no KeyOS API versions");
    }

    let mut has_default = false;
    for entry in &manifest.versions {
        let path = entry.path.trim_end_matches('/');
        let mut components = Path::new(path).components();
        let safe_path =
            matches!(components.next(), Some(std::path::Component::Normal(_))) && components.next().is_none();
        if !safe_path || !root.join(path).join("index.html").is_file() {
            bail!(
                "The SDK docs manifest references an invalid or missing KeyOS {} snapshot at '{}'",
                entry.keyos_version,
                entry.path
            );
        }
        has_default |= entry.keyos_version == manifest.default_keyos_version;
    }
    if !has_default {
        bail!("The SDK docs manifest default KeyOS version is not included in the bundle");
    }
    Ok(())
}

fn version_from_bundle_name(name: &str, target: &str) -> Option<Version> {
    let version = name.strip_prefix("foundation-sdk-")?.strip_suffix(&format!("-{target}"))?;
    Version::parse(version).ok()
}

fn bundle_version(name: &str) -> Option<Version> {
    SDK_HOST_TARGETS.iter().find_map(|target| version_from_bundle_name(name, target))
}

fn host_target() -> Result<String> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => bail!("Unsupported SDK host architecture: {other}"),
    };
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        other => bail!("Unsupported SDK host operating system: {other}"),
    };
    Ok(format!("{arch}-{os}"))
}

fn sdk_install_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FOUNDATION_SDK_INSTALL_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(dirs::home_dir().context("Could not determine the home directory")?.join(".foundation").join("sdk"))
}

fn file_url(path: &Path) -> Result<String> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("Could not resolve documentation file {}", path.display()))?;
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            url.push(*byte as char);
        } else {
            use std::fmt::Write;
            write!(url, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    Ok(url)
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(url).status().context("Could not run the macOS 'open' command")?;

    #[cfg(target_os = "linux")]
    let status = Command::new("xdg-open").arg(url).status().context("Could not run 'xdg-open'")?;

    #[cfg(target_os = "windows")]
    let status = Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()
        .context("Could not run the Windows browser launcher")?;

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let status = {
        bail!("Opening a browser is not supported on this operating system");
    };

    if !status.success() {
        bail!("The system browser launcher exited with {status}");
    }
    Ok(())
}

fn installed_sdk_names(install_root: &Path) -> String {
    let mut names = fs::read_dir(install_root)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("foundation-sdk-"))
        .collect::<Vec<_>>();
    names.sort();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_version_from_bundle_directory() {
        assert_eq!(
            version_from_bundle_name(
                "foundation-sdk-1.2.3-beta.1-aarch64-apple-darwin",
                "aarch64-apple-darwin"
            ),
            Some(Version::parse("1.2.3-beta.1").unwrap())
        );
    }

    #[test]
    fn rejects_other_bundle_targets_when_parsing_version() {
        assert_eq!(
            version_from_bundle_name("foundation-sdk-1.2.3-x86_64-unknown-linux-gnu", "aarch64-apple-darwin"),
            None
        );
    }

    #[test]
    fn release_version_does_not_match_a_prerelease_bundle() {
        let requested = Version::parse("1.2.3").unwrap();
        let installed = bundle_version("foundation-sdk-1.2.3-beta.1-aarch64-apple-darwin");

        assert_ne!(installed.as_ref(), Some(&requested));
    }

    #[test]
    fn accepts_a_complete_versioned_docs_bundle() {
        let docs = tempfile::tempdir().unwrap();
        fs::create_dir(docs.path().join("v1.4.0")).unwrap();
        fs::write(docs.path().join("index.html"), "bundle").unwrap();
        fs::write(docs.path().join("bundle-manifest.js"), "manifest").unwrap();
        fs::write(docs.path().join("version-selector.js"), "selector").unwrap();
        fs::write(docs.path().join("v1.4.0/index.html"), "docs").unwrap();
        fs::write(
            docs.path().join("bundle-manifest.json"),
            r#"{"schemaVersion":1,"defaultKeyosVersion":"1.4.0","versions":[{"keyosVersion":"1.4.0","path":"v1.4.0/"}]}"#,
        )
        .unwrap();

        validate_docs_bundle(docs.path(), None).unwrap();
    }

    #[test]
    fn rejects_unversioned_crate_only_docs() {
        let docs = tempfile::tempdir().unwrap();
        fs::create_dir(docs.path().join("foundation_manifest")).unwrap();
        fs::write(docs.path().join("foundation_manifest/index.html"), "docs").unwrap();

        assert!(validate_docs_bundle(docs.path(), None).unwrap_err().to_string().contains("incomplete"));
    }

    #[test]
    fn file_urls_are_percent_encoded() {
        let docs = tempfile::tempdir().unwrap();
        let page = docs.path().join("API docs #1.html");
        fs::write(&page, "docs").unwrap();

        let url = file_url(&page).unwrap();
        assert!(url.starts_with("file:///"));
        assert!(url.ends_with("API%20docs%20%231.html"));
    }

    #[test]
    fn sdk_root_always_supplies_the_current_docs() {
        let (label, root) = resolve_default_docs_root(
            PathBuf::from("/installed/current/docs/api"),
            None,
            Some(PathBuf::from("/development/sdk")),
        );

        assert_eq!(label, "current");
        assert_eq!(root, PathBuf::from("/development/sdk/docs/api"));
    }
}
