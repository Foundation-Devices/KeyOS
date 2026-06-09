// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Project context discovery

use std::path::{Path, PathBuf};

use crate::config::{AppConfig, APP_CONFIG_FILE};

#[derive(Debug)]
pub struct ProjectContext {
    /// Absolute path to project root (directory containing app-config.toml)
    pub root: PathBuf,

    /// Parsed app-config.toml
    pub config: AppConfig,

    /// Path to build output directory
    pub build_dir: PathBuf,

    /// Path to i18n directory (if exists)
    pub i18n_dir: Option<PathBuf>,
}

impl ProjectContext {
    /// Discover project by walking up from current directory
    pub fn discover() -> Result<Self, ContextError> {
        let cwd = std::env::current_dir().map_err(ContextError::CurrentDirError)?;
        Self::discover_from(&cwd)
    }

    /// Discover project by walking up from specified directory
    pub fn discover_from(start: &Path) -> Result<Self, ContextError> {
        let mut current = start.to_path_buf();

        loop {
            let config_path = current.join(APP_CONFIG_FILE);
            if config_path.exists() {
                let config = AppConfig::load(&config_path)?;
                config.validate(&current)?;

                let build_dir = current.join("target");
                let i18n_dir = {
                    let dir = current.join("i18n");
                    dir.exists().then_some(dir)
                };

                return Ok(Self { root: current, config, build_dir, i18n_dir });
            }

            match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => return Err(ContextError::NotFound),
            }
        }
    }

    /// Try to discover project, returning None if not found
    pub fn discover_optional() -> Option<Self> { Self::discover().ok() }

    /// Get path relative to project root
    pub fn relative_path(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.root).map(|p| p.to_path_buf()).unwrap_or_else(|_| path.to_path_buf())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("Could not determine current directory: {0}")]
    CurrentDirError(std::io::Error),

    #[error("No app-config.toml found in current directory or any parent")]
    NotFound,

    #[error(transparent)]
    ConfigError(#[from] crate::config::ConfigError),
}
