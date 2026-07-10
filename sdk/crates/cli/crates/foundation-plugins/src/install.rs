// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Plugin installation from local registry or GitHub

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Environment variable to override the default plugin index path
const INDEX_ENV_VAR: &str = "FOUNDATION_PLUGIN_INDEX";

/// Default index filename
const DEFAULT_INDEX_FILE: &str = "plugin-index.toml";

/// Default GitHub API base URL for release lookup
const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";

/// Current supported plugin-index schema version. If the index file ships with a
/// newer version we refuse to use it rather than silently misinterpret entries.
const SUPPORTED_INDEX_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
struct PluginIndex {
    version: u32,
    plugins: Vec<PluginEntry>,
}

impl PluginIndex {
    fn validate_version(&self) -> Result<(), InstallError> {
        if self.version > SUPPORTED_INDEX_VERSION {
            Err(InstallError::UnsupportedIndexVersion {
                got: self.version,
                supported: SUPPORTED_INDEX_VERSION,
            })
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginEntry {
    pub name: String,
    pub description: String,
    pub repository: String,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

pub struct PluginInstaller {
    bin_dir: PathBuf,
    index_path: PathBuf,
    cache_path: PathBuf,
    github_api_base: String,
    client: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallTarget {
    name: String,
    repository: String,
}

impl PluginInstaller {
    /// Create a new plugin installer with default paths
    /// - Plugins installed to: ~/.foundation/plugins/
    /// - Index read from: ~/.foundation/plugin-index.toml (or FOUNDATION_PLUGIN_INDEX env var)
    pub fn new() -> Self {
        let foundation_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".foundation");

        let bin_dir = foundation_dir.join("plugins");

        let index_path = std::env::var(INDEX_ENV_VAR)
            .map(PathBuf::from)
            .unwrap_or_else(|_| foundation_dir.join(DEFAULT_INDEX_FILE));

        Self::from_parts(
            bin_dir,
            index_path,
            crate::cache::PluginCache::path(),
            DEFAULT_GITHUB_API_BASE.to_string(),
        )
    }

    /// Create a plugin installer with a custom index path
    pub fn with_index(index_path: impl Into<PathBuf>) -> Self {
        let foundation_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".foundation");

        Self::from_parts(
            foundation_dir.join("plugins"),
            index_path.into(),
            crate::cache::PluginCache::path(),
            DEFAULT_GITHUB_API_BASE.to_string(),
        )
    }

    /// Get the bin directory path
    pub fn bin_dir(&self) -> &PathBuf { &self.bin_dir }

    /// Get the index path
    pub fn index_path(&self) -> &PathBuf { &self.index_path }

    /// Install a plugin by name or repo
    /// - "foo" → looks up in index, installs from GitHub
    /// - "user/repo" → direct GitHub install
    pub async fn install(&self, spec: &str) -> Result<InstalledPlugin, InstallError> {
        let target = self.resolve_install_target(spec).await?;
        let release = self.fetch_latest_release(&target.repository).await?;
        let asset = self.find_platform_asset(&release)?;
        eprintln!("Installing {} (release {})...", target.name, release.tag_name);
        let path = self.download_and_install(&target.name, asset).await?;

        Ok(InstalledPlugin { name: target.name, path })
    }

    /// Search the plugin index
    pub async fn search(&self, query: &str) -> Result<Vec<PluginEntry>, InstallError> {
        let index = self.fetch_index().await?;

        let query_lower = query.to_lowercase();
        let results = index
            .plugins
            .into_iter()
            .filter(|p| {
                p.name.to_lowercase().contains(&query_lower)
                    || p.description.to_lowercase().contains(&query_lower)
                    || p.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
            })
            .collect();

        Ok(results)
    }

    /// List all plugins in the index
    pub async fn list_available(&self) -> Result<Vec<PluginEntry>, InstallError> {
        let index = self.fetch_index().await?;
        Ok(index.plugins)
    }

    /// List installed plugins
    pub fn list_installed(&self) -> Result<Vec<String>, InstallError> {
        let mut plugins = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&self.bin_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if let Some(plugin_name) = name_str.strip_prefix("foundation-") {
                    let plugin_name =
                        plugin_name.strip_suffix(std::env::consts::EXE_SUFFIX).unwrap_or(plugin_name);
                    plugins.push(plugin_name.to_string());
                }
            }
        }

        Ok(plugins)
    }

    /// Uninstall a plugin
    pub fn uninstall(&self, name: &str) -> Result<(), InstallError> {
        let binary_name = format!("foundation-{}{}", name, std::env::consts::EXE_SUFFIX);
        let path = self.bin_dir.join(&binary_name);

        if !path.exists() {
            return Err(InstallError::NotInstalled(name.to_string()));
        }

        std::fs::remove_file(&path)?;

        // Clear from plugin cache
        let mut cache = crate::cache::PluginCache::load_from(&self.cache_path);
        cache.commands.remove(name);
        cache.save_to(&self.cache_path)?;

        Ok(())
    }

    /// Fetch and parse the plugin index from local file
    async fn fetch_index(&self) -> Result<PluginIndex, InstallError> {
        let path = &self.index_path;

        // Handle file:// URLs by stripping the prefix
        let path = if let Some(stripped) = path.to_string_lossy().strip_prefix("file://") {
            PathBuf::from(stripped)
        } else {
            path.clone()
        };

        let content = std::fs::read_to_string(&path)
            .map_err(|e| InstallError::IndexReadError(path.display().to_string(), e.to_string()))?;

        let index: PluginIndex = toml::from_str(&content)
            .map_err(|e| InstallError::IndexParseError(path.display().to_string(), e.to_string()))?;

        index.validate_version()?;
        Ok(index)
    }

    async fn lookup_in_index(&self, name: &str) -> Result<PluginEntry, InstallError> {
        let index = self.fetch_index().await?;

        index
            .plugins
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .ok_or_else(|| InstallError::NotFound(name.to_string()))
    }

    async fn resolve_install_target(&self, spec: &str) -> Result<InstallTarget, InstallError> {
        if spec.contains('/') {
            let name = plugin_name_from_repo(spec)?;
            return Ok(InstallTarget { name, repository: spec.to_string() });
        }

        let entry = self.lookup_in_index(spec).await?;
        Ok(InstallTarget { name: entry.name, repository: entry.repository })
    }

    async fn fetch_latest_release(&self, repo: &str) -> Result<GitHubRelease, InstallError> {
        let url = format!("{}/repos/{}/releases/latest", self.github_api_base.trim_end_matches('/'), repo);

        let response = self.client.get(&url).header("User-Agent", "foundation-cli").send().await?;

        if !response.status().is_success() {
            return Err(InstallError::ReleaseNotFound(repo.to_string()));
        }

        let release = response.json().await?;
        Ok(release)
    }

    fn find_platform_asset<'a>(&self, release: &'a GitHubRelease) -> Result<&'a GitHubAsset, InstallError> {
        let target = current_target();
        let expected_suffix = format!("-{}{}", target, std::env::consts::EXE_SUFFIX);

        release
            .assets
            .iter()
            .find(|a| a.name.ends_with(&expected_suffix))
            .ok_or_else(|| InstallError::NoPlatformBinary(target.to_string()))
    }

    async fn download_and_install(&self, name: &str, asset: &GitHubAsset) -> Result<PathBuf, InstallError> {
        // Ensure bin directory exists
        std::fs::create_dir_all(&self.bin_dir)?;

        // Download binary
        let response = self.client.get(&asset.browser_download_url).send().await?.error_for_status()?;
        let bytes = response.bytes().await?;

        // Determine install path
        let binary_name = format!("foundation-{}{}", name, std::env::consts::EXE_SUFFIX);
        let install_path = self.bin_dir.join(&binary_name);

        install_binary_atomically(&install_path, &bytes)?;

        // Update plugin cache
        let mut cache = crate::cache::PluginCache::load_from(&self.cache_path);
        cache.commands.insert(name.to_string(), install_path);
        cache.save_to(&self.cache_path)?;

        Ok(self.bin_dir.join(binary_name))
    }

    fn from_parts(
        bin_dir: PathBuf,
        index_path: PathBuf,
        cache_path: PathBuf,
        github_api_base: String,
    ) -> Self {
        Self { bin_dir, index_path, cache_path, github_api_base, client: reqwest::Client::new() }
    }

    #[cfg(test)]
    fn for_test(
        bin_dir: impl Into<PathBuf>,
        index_path: impl Into<PathBuf>,
        cache_path: impl Into<PathBuf>,
        github_api_base: impl Into<String>,
    ) -> Self {
        Self::from_parts(bin_dir.into(), index_path.into(), cache_path.into(), github_api_base.into())
    }
}

impl Default for PluginInstaller {
    fn default() -> Self { Self::new() }
}

/// Get current platform's target triple
fn current_target() -> &'static str {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        "x86_64-unknown-linux-gnu"
    }

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        "aarch64-unknown-linux-gnu"
    }

    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    {
        "x86_64-apple-darwin"
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        "aarch64-apple-darwin"
    }

    #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
    {
        "x86_64-pc-windows-msvc"
    }

    #[cfg(not(any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux"),
        all(target_arch = "x86_64", target_os = "macos"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "windows")
    )))]
    {
        "unknown"
    }
}

fn plugin_name_from_repo(repo: &str) -> Result<String, InstallError> {
    let repo_name = repo.rsplit('/').next().unwrap_or_default().trim();
    if repo_name.is_empty() {
        return Err(InstallError::InvalidPluginSpec(repo.to_string()));
    }

    Ok(repo_name.strip_prefix("foundation-").unwrap_or(repo_name).to_string())
}

fn install_binary_atomically(install_path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    let parent = install_path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("install path has no parent: {}", install_path.display()),
        )
    })?;
    let file_name = install_path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("install path has no filename: {}", install_path.display()),
        )
    })?;
    let temp_path = parent.join(format!(".{}.{}.tmp", file_name.to_string_lossy(), std::process::id()));

    let result = (|| -> Result<(), InstallError> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp_path)?;
        file.write_all(bytes)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = file.metadata()?.permissions();
            perms.set_mode(0o755);
            file.set_permissions(perms)?;
        }

        file.sync_all()?;
        drop(file);

        std::fs::rename(&temp_path, install_path)?;
        sync_parent_dir(parent);
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }

    result
}

fn sync_parent_dir(path: &Path) {
    if let Ok(dir) = std::fs::File::open(path) {
        let _ = dir.sync_all();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("Plugin not found in registry: {0}")]
    NotFound(String),

    #[error("Plugin not installed: {0}")]
    NotInstalled(String),

    #[error("Invalid plugin spec: {0}")]
    InvalidPluginSpec(String),

    #[error("No release found for repository: {0}")]
    ReleaseNotFound(String),

    #[error("No binary available for platform: {0}")]
    NoPlatformBinary(String),

    #[error("Failed to read index file '{0}': {1}")]
    IndexReadError(String, String),

    #[error("Failed to parse index file '{0}': {1}")]
    IndexParseError(String, String),

    #[error(
        "Plugin index version {got} is newer than supported version {supported}; upgrade the foundation CLI"
    )]
    UnsupportedIndexVersion { got: u32, supported: u32 },

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::{current_target, install_binary_atomically, plugin_name_from_repo, PluginInstaller};

    #[test]
    fn strips_foundation_prefix_from_repo_name() {
        assert_eq!(plugin_name_from_repo("foundation-devices/foundation-foo").unwrap(), "foo");
        assert_eq!(plugin_name_from_repo("foundation-devices/bar").unwrap(), "bar");
    }

    #[test]
    fn atomic_binary_install_replaces_existing_file() {
        let temp_root_dir = tempfile::tempdir().unwrap();
        let temp_root = temp_root_dir.path();
        let install_path = temp_root.join("foundation-demo");
        fs::write(&install_path, b"old-plugin").unwrap();

        install_binary_atomically(&install_path, b"new-plugin").unwrap();

        assert_eq!(fs::read(&install_path).unwrap(), b"new-plugin");
        let leftovers = fs::read_dir(temp_root)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty());
    }

    #[tokio::test]
    async fn installs_plugin_from_mocked_release_server() {
        let temp_root_dir = tempfile::tempdir().unwrap();
        let temp_root = temp_root_dir.path();
        let bin_dir = temp_root.join("plugins");
        let cache_path = temp_root.join("plugin-cache.json");

        let index_path = temp_root.join("plugin-index.toml");
        fs::write(
            &index_path,
            r#"
            version = 1

            [[plugins]]
            name = "demo"
            description = "Demo plugin"
            repository = "acme/demo"
            "#,
        )
        .unwrap();

        let asset_name = format!("foundation-demo-{}{}", current_target(), std::env::consts::EXE_SUFFIX);
        let server = MockReleaseServer::spawn(asset_name.clone(), 200, b"plugin-binary".to_vec());
        let installer = PluginInstaller::for_test(&bin_dir, index_path, &cache_path, server.base_url());

        let installed = installer.install("demo").await.unwrap();

        assert_eq!(installed.name, "demo");
        assert_eq!(installed.path, bin_dir.join(format!("foundation-demo{}", std::env::consts::EXE_SUFFIX)));
        assert_eq!(fs::read(&installed.path).unwrap(), b"plugin-binary");
    }

    #[tokio::test]
    async fn rejects_failed_plugin_asset_download_before_installing() {
        let temp_root_dir = tempfile::tempdir().unwrap();
        let temp_root = temp_root_dir.path();
        let bin_dir = temp_root.join("plugins");
        let cache_path = temp_root.join("plugin-cache.json");

        let index_path = temp_root.join("plugin-index.toml");
        fs::write(
            &index_path,
            r#"
            version = 1

            [[plugins]]
            name = "demo"
            description = "Demo plugin"
            repository = "acme/demo"
            "#,
        )
        .unwrap();

        let asset_name = format!("foundation-demo-{}{}", current_target(), std::env::consts::EXE_SUFFIX);
        let server = MockReleaseServer::spawn(asset_name, 404, b"not found".to_vec());
        let installer = PluginInstaller::for_test(&bin_dir, index_path, &cache_path, server.base_url());

        let err = installer.install("demo").await.unwrap_err();

        assert!(matches!(err, super::InstallError::Network(_)));
        assert!(!installer
            .bin_dir()
            .join(format!("foundation-demo{}", std::env::consts::EXE_SUFFIX))
            .exists());
        assert!(crate::cache::PluginCache::load_from(&cache_path).commands.get("demo").is_none());
    }

    struct MockReleaseServer {
        base_url: String,
        join_handle: Option<thread::JoinHandle<()>>,
    }

    impl MockReleaseServer {
        fn spawn(asset_name: String, asset_status: u16, asset_bytes: Vec<u8>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());

            let release_body = format!(
                r#"{{
                    "tag_name": "v1.0.0",
                    "assets": [
                        {{
                            "name": "{asset_name}",
                            "browser_download_url": "{base_url}/downloads/{asset_name}"
                        }}
                    ]
                }}"#
            );

            let join_handle = thread::spawn(move || {
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut buffer = [0u8; 4096];
                    let bytes_read = stream.read(&mut buffer).unwrap();
                    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
                    let path =
                        request.lines().next().and_then(|line| line.split_whitespace().nth(1)).unwrap_or("/");

                    let (status, content_type, body) = if path == "/repos/acme/demo/releases/latest" {
                        (200, "application/json", release_body.as_bytes().to_vec())
                    } else if path == format!("/downloads/{asset_name}") {
                        (asset_status, "application/octet-stream", asset_bytes.clone())
                    } else {
                        (404, "text/plain", b"not found".to_vec())
                    };
                    let reason = match status {
                        200 => "OK",
                        404 => "Not Found",
                        _ => "Error",
                    };

                    write!(
                        stream,
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                        body.len(),
                        content_type,
                    )
                    .unwrap();
                    stream.write_all(&body).unwrap();
                }
            });

            Self { base_url, join_handle: Some(join_handle) }
        }

        fn base_url(&self) -> &str { &self.base_url }
    }

    impl Drop for MockReleaseServer {
        fn drop(&mut self) {
            if let Some(handle) = self.join_handle.take() {
                handle.join().unwrap();
            }
        }
    }
}
