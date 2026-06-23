// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Plugin cache management

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginCache {
    pub version: u32,
    pub commands: HashMap<String, PathBuf>,
}

impl PluginCache {
    /// Get the cache file path
    pub fn path() -> PathBuf {
        dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("foundation").join("plugin-cache.json")
    }

    /// Load the cache from its default path. See [`load_from`](Self::load_from).
    pub fn load() -> Self { Self::load_from(&Self::path()) }

    /// Load cache from `path`, or return empty cache. Logs a warning to stderr
    /// when the cache exists but can't be parsed or has the wrong version, so
    /// users see why their plugin list got rebuilt unexpectedly.
    pub fn load_from(path: &Path) -> Self {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::empty(),
            Err(e) => {
                eprintln!(
                    "warning: could not read plugin cache at {}: {e}; rebuilding from scratch",
                    path.display()
                );
                return Self::empty();
            }
        };

        match serde_json::from_str::<Self>(&contents) {
            Ok(cache) if cache.version == CACHE_VERSION => cache,
            Ok(cache) => {
                eprintln!(
                    "warning: plugin cache version {} doesn't match expected {CACHE_VERSION}; rebuilding",
                    cache.version
                );
                Self::empty()
            }
            Err(e) => {
                eprintln!(
                    "warning: plugin cache at {} is malformed: {e}; rebuilding from scratch",
                    path.display()
                );
                Self::empty()
            }
        }
    }

    /// Create an empty cache
    pub fn empty() -> Self { Self { version: CACHE_VERSION, commands: HashMap::new() } }

    /// Save the cache to its default path. See [`save_to`](Self::save_to).
    pub fn save(&self) -> std::io::Result<()> { self.save_to(&Self::path()) }

    /// Save cache to `path`, creating parent directories as needed.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        std::fs::write(path, json)
    }

    /// Check if a command exists in cache and its binary is present
    pub fn get(&self, name: &str) -> Option<&PathBuf> { self.commands.get(name).filter(|p| p.is_file()) }

    /// Update cache with new commands (replaces all entries)
    pub fn update(&mut self, commands: HashMap<String, PathBuf>) { self.commands = commands; }
}
