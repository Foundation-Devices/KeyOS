// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Foundation SDK root discovery and path resolution.

use std::env;
use std::path::{Path, PathBuf};

/// Environment variable exported by the Foundation SDK flake.
pub const SDK_ROOT_ENV: &str = "FOUNDATION_SDK_ROOT";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdkLayout {
    Repo,
    Bundle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdkRoot {
    root: PathBuf,
    layout: SdkLayout,
}

impl SdkRoot {
    /// Discover the SDK root from the shell environment, current directory, or current executable.
    pub fn discover() -> Result<Self, SdkError> {
        if let Some(root) = env::var_os(SDK_ROOT_ENV) {
            return Self::from_root(PathBuf::from(root));
        }

        let cwd = env::current_dir().map_err(SdkError::CurrentDir)?;
        if let Ok(root) = Self::discover_from(&cwd) {
            return Ok(root);
        }

        let current_exe = env::current_exe().map_err(SdkError::CurrentExe)?;
        if let Ok(root) = Self::discover_from(&current_exe) {
            return Ok(root);
        }

        Err(SdkError::NotFound)
    }

    /// Discover the SDK root by walking up from a known path.
    pub fn discover_from(start: &Path) -> Result<Self, SdkError> {
        find_sdk_root(start).ok_or(SdkError::NotFound)
    }

    /// Validate and classify a specific SDK root path. Canonicalizes the path
    /// so that callers downstream see one consistent representation regardless
    /// of symlinks (common with Nix store paths).
    pub fn from_root(root: PathBuf) -> Result<Self, SdkError> {
        let canonical = std::fs::canonicalize(&root).unwrap_or(root);
        match classify_root(&canonical) {
            Some(layout) => Ok(Self { root: canonical, layout }),
            None => Err(SdkError::InvalidRoot(canonical)),
        }
    }

    pub fn root(&self) -> &Path { &self.root }

    pub fn layout(&self) -> SdkLayout { self.layout }

    pub fn keyos_root(&self) -> PathBuf {
        match self.layout {
            SdkLayout::Repo => self.root.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()),
            SdkLayout::Bundle => self.root.join("lib").join("keyos"),
        }
    }

    pub fn ui_library_path(&self) -> PathBuf {
        match self.layout {
            SdkLayout::Repo => self.keyos_root().join("ui2").join("components").join("ui"),
            SdkLayout::Bundle => self.root.join("ui").join("ui"),
        }
    }

    pub fn ui_shared_resources_path(&self) -> PathBuf {
        match self.layout {
            SdkLayout::Repo => self.keyos_root().join("ui2").join("resources"),
            SdkLayout::Bundle => self.root.join("resources"),
        }
    }

    /// Directory of plugin component schemas (`button.json`, …), used by the
    /// theme-compile step to generate per-app component themes. Lives under the
    /// KeyOS tree in both layouts (Repo: source tree; Bundle: the staged
    /// `lib/keyos` copy — see the matching `[[copy]]` in sdk-build.toml).
    pub fn plugin_schema_path(&self) -> PathBuf {
        self.keyos_root().join("ui2").join("theme-editor").join("defaults").join("plugins")
    }

    pub fn template_root(&self) -> Option<PathBuf> {
        self.template_roots().into_iter().find(|path| path.exists())
    }

    pub fn template_roots(&self) -> Vec<PathBuf> {
        match self.layout {
            SdkLayout::Repo => {
                vec![self.root.join("crates").join("cli").join("templates"), self.root.join("templates")]
            }
            SdkLayout::Bundle => vec![self.root.join("lib").join("templates")],
        }
    }

    pub fn bundled_binary(&self, name: &str) -> Option<PathBuf> {
        let path = self.root.join("bin").join(name);
        path.exists().then_some(path)
    }

    /// Find the first matching tool by name, preferring an SDK-bundled binary
    /// over one on PATH. Logs the resolved binary to stderr when an environment
    /// variable opts the user in, so the user can audit which `foundation-*`
    /// got picked up. Set `FOUNDATION_VERBOSE_TOOL_RESOLUTION=1` to enable.
    pub fn tool_path(&self, names: &[&str]) -> Option<PathBuf> {
        let verbose = env::var("FOUNDATION_VERBOSE_TOOL_RESOLUTION").is_ok();

        for name in names {
            if let Some(path) = self.bundled_binary(name) {
                if verbose {
                    eprintln!("tool_path: using bundled {name} at {}", path.display());
                }
                return Some(path);
            }
        }

        for name in names {
            if let Some(path) = find_in_path(name) {
                if verbose {
                    eprintln!("tool_path: using PATH {name} at {}", path.display());
                }
                return Some(path);
            }
        }

        None
    }
}

fn classify_root(root: &Path) -> Option<SdkLayout> {
    if root.join("flake.nix").exists()
        && root.join("sdk-build.toml").exists()
        && root.parent().map(|path| path.join("Cargo.toml").exists()) == Some(true)
    {
        return Some(SdkLayout::Repo);
    }

    if root.join("flake.nix").exists() && root.join("bin").is_dir() && root.join("lib").join("keyos").is_dir()
    {
        return Some(SdkLayout::Bundle);
    }

    None
}

fn find_sdk_root(start: &Path) -> Option<SdkRoot> {
    let mut current = if start.is_file() { start.parent()?.to_path_buf() } else { start.to_path_buf() };

    loop {
        if let Some(layout) = classify_root(&current) {
            // Canonicalize on discovery so cache keys / equality comparisons
            // line up regardless of the path the user walked from.
            let root = std::fs::canonicalize(&current).unwrap_or(current);
            return Some(SdkRoot { root, layout });
        }

        if !current.pop() {
            return None;
        }
    }
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    let command = Path::new(command);
    if command.is_absolute() && command.exists() {
        return Some(command.to_path_buf());
    }

    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return Some(candidate);
        }

        if cfg!(windows) {
            for ext in [".exe", ".cmd", ".bat"] {
                let candidate = dir.join(format!("{}{}", command.to_string_lossy(), ext));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }

        None
    })
}

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("Could not determine current directory: {0}")]
    CurrentDir(std::io::Error),

    #[error("Could not determine current executable: {0}")]
    CurrentExe(std::io::Error),

    #[error("FOUNDATION_SDK_ROOT points to an invalid SDK root: {0}")]
    InvalidRoot(PathBuf),

    #[error(
        "Could not locate the Foundation SDK root. Run from an SDK checkout or unpacked SDK bundle, or set FOUNDATION_SDK_ROOT."
    )]
    NotFound,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{SdkLayout, SdkRoot};

    #[test]
    fn discovers_repo_layout() {
        let repo_root_dir = tempfile::tempdir().unwrap();
        let repo_root = repo_root_dir.path();
        fs::write(repo_root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let root = repo_root.join("sdk");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("flake.nix"), "{}").unwrap();
        fs::write(root.join("sdk-build.toml"), "").unwrap();
        fs::create_dir_all(root.join("crates").join("cli")).unwrap();

        let nested = root.join("crates").join("cli").join("src");
        fs::create_dir_all(&nested).unwrap();

        let sdk = SdkRoot::discover_from(&nested).unwrap();
        assert_eq!(sdk.layout(), SdkLayout::Repo);
        assert_eq!(sdk.root(), root.as_path());
        assert_eq!(sdk.keyos_root(), repo_root);
        assert_eq!(sdk.ui_library_path(), sdk.keyos_root().join("ui2").join("components").join("ui"));
        assert_eq!(sdk.ui_shared_resources_path(), sdk.keyos_root().join("ui2").join("resources"));
    }

    #[test]
    fn discovers_bundle_layout() {
        let root_dir = tempfile::tempdir().unwrap();
        let root = root_dir.path();
        fs::write(root.join("flake.nix"), "{}").unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("lib").join("keyos")).unwrap();

        let nested = root.join("bin").join("foundation");
        fs::write(&nested, "").unwrap();

        let sdk = SdkRoot::discover_from(&nested).unwrap();
        assert_eq!(sdk.layout(), SdkLayout::Bundle);
        assert_eq!(sdk.root(), root);
        assert_eq!(sdk.keyos_root(), root.join("lib").join("keyos"));
        assert_eq!(sdk.ui_library_path(), root.join("ui").join("ui"));
        assert_eq!(sdk.ui_shared_resources_path(), root.join("resources"));
    }
}
