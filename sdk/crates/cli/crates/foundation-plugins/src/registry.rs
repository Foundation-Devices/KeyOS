// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Command registry with plugin discovery

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use foundation_plugin_sdk::Command;

use crate::cache::PluginCache;
use crate::install::PluginInstaller;

pub struct CommandRegistry {
    builtins: HashMap<String, Box<dyn Command>>,
    cache: PluginCache,
}

pub enum ResolvedCommand<'a> {
    Builtin(&'a dyn Command),
    External(PathBuf),
}

impl CommandRegistry {
    pub fn new() -> Self { Self { builtins: HashMap::new(), cache: PluginCache::load() } }

    /// Register a built-in command
    pub fn register(&mut self, command: Box<dyn Command>) {
        self.builtins.insert(command.name().to_string(), command);
    }

    /// Resolve a command by name
    pub fn resolve(&mut self, name: &str) -> Option<ResolvedCommand<'_>> {
        // 1. Check builtins first
        if self.builtins.contains_key(name) {
            return self.builtins.get(name).map(|c| ResolvedCommand::Builtin(c.as_ref()));
        }

        // 2. Check cache
        if let Some(path) = self.cache.get(name) {
            return Some(ResolvedCommand::External(path.clone()));
        }

        // 3. Not in cache or binary missing - rescan
        self.rescan();

        // 4. Check cache again after rescan
        self.cache.get(name).map(|p| ResolvedCommand::External(p.clone()))
    }

    /// Get all available commands (triggers rescan)
    pub fn all_commands(&mut self) -> Vec<String> {
        self.rescan();

        let mut commands: Vec<String> = self.builtins.keys().cloned().collect();
        commands.extend(self.cache.commands.keys().cloned());
        commands.sort();
        commands.dedup();
        commands
    }

    /// Get all built-in commands
    pub fn builtin_commands(&self) -> impl Iterator<Item = &dyn Command> {
        self.builtins.values().map(|c| c.as_ref())
    }

    /// Scan PATH for foundation-* binaries and update cache
    fn rescan(&mut self) {
        let mut commands = HashMap::new();

        scan_plugin_dir(&PluginInstaller::new().bin_dir().clone(), &self.builtins, &mut commands);

        if let Some(path_var) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_var) {
                scan_plugin_dir(&dir, &self.builtins, &mut commands);
            }
        }

        self.cache.update(commands);
        let _ = self.cache.save();
    }
}

fn scan_plugin_dir(
    dir: &Path,
    builtins: &HashMap<String, Box<dyn Command>>,
    commands: &mut HashMap<String, PathBuf>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        if let Some(cmd_name) = name_str.strip_prefix("foundation-") {
            let cmd_name = cmd_name.strip_suffix(std::env::consts::EXE_SUFFIX).unwrap_or(cmd_name);

            if builtins.contains_key(cmd_name) {
                continue;
            }

            commands.entry(cmd_name.to_string()).or_insert_with(|| entry.path());
        }
    }
}

impl Default for CommandRegistry {
    fn default() -> Self { Self::new() }
}
