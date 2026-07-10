// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::cmp::Reverse;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{boxed_err, Config, Result};
use crate::util;

pub type SourceOverrides = BTreeMap<String, PathBuf>;
const KEYOS_SOURCE_NAME: &str = "keyos";
const KEYOS_OVERRIDE_ENV: &str = "KEYOS_DIR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedFrom {
    Submodule,
    Override,
}

#[derive(Clone, Debug)]
pub struct ResolvedSubmodule {
    configured_path: String,
    path: PathBuf,
    resolved_from: ResolvedFrom,
}

impl ResolvedSubmodule {
    pub fn configured_path(&self) -> &str { &self.configured_path }

    pub fn path(&self) -> &Path { &self.path }

    pub fn resolved_from(&self) -> &ResolvedFrom { &self.resolved_from }
}

#[derive(Clone, Debug)]
pub struct SourceResolver {
    keyos_root: PathBuf,
    submodules: BTreeMap<String, ResolvedSubmodule>,
}

impl SourceResolver {
    pub fn keyos_root(&self) -> &Path { &self.keyos_root }

    pub fn submodule(&self, name: &str) -> Option<&ResolvedSubmodule> { self.submodules.get(name) }

    pub fn resolve_source(&self, root: &Path, source: &str) -> PathBuf {
        let mut resolved = self.submodules.values().collect::<Vec<_>>();
        resolved.sort_by_key(|entry| Reverse(entry.configured_path.len()));

        for entry in resolved {
            if source == entry.configured_path() {
                return entry.path().to_path_buf();
            }

            if let Some(remainder) = source.strip_prefix(&format!("{}/", entry.configured_path())) {
                return entry.path().join(remainder);
            }
        }

        if source == ".." {
            return self.keyos_root.clone();
        }

        if let Some(remainder) = source.strip_prefix("../") {
            return self.keyos_root.join(remainder);
        }

        root.join(source)
    }
}

/// Fill `overrides` from the environment for any source not already set, so
/// explicit flags win over `KEYOS_DIR` and the per-submodule env overrides.
pub fn apply_env_overrides(config: &Config, overrides: &mut SourceOverrides) {
    if let Entry::Vacant(entry) = overrides.entry(KEYOS_SOURCE_NAME.to_string()) {
        if let Some(path) = env::var_os(KEYOS_OVERRIDE_ENV) {
            entry.insert(PathBuf::from(path));
        }
    }

    for (name, submodule) in &config.submodules {
        if let Entry::Vacant(entry) = overrides.entry(name.clone()) {
            if let Some(path) = env::var_os(&submodule.env_override) {
                entry.insert(PathBuf::from(path));
            }
        }
    }
}

pub fn resolve(root: &Path, config: &Config, overrides: &SourceOverrides) -> Result<SourceResolver> {
    let mut submodules = BTreeMap::new();
    let keyos_root = active_keyos_root(root, overrides.get(KEYOS_SOURCE_NAME).map(PathBuf::as_path))?;

    for (name, submodule) in &config.submodules {
        let override_path = absolute_override(root, overrides.get(name).map(PathBuf::as_path));
        let resolved_from =
            if override_path.is_some() { ResolvedFrom::Override } else { ResolvedFrom::Submodule };
        let path = override_path.unwrap_or_else(|| root.join(&submodule.path));

        submodules.insert(
            name.clone(),
            ResolvedSubmodule { configured_path: submodule.path.clone(), path, resolved_from },
        );
    }

    Ok(SourceResolver { keyos_root, submodules })
}

pub fn check_all(root: &Path, config: &Config, overrides: &SourceOverrides) -> Result<()> {
    let gitmodules = root.join(".gitmodules");
    let contents = gitmodules.exists().then(|| fs::read_to_string(&gitmodules)).transpose()?;
    let mut errors = Vec::new();

    if let Some(path) = absolute_override(root, overrides.get(KEYOS_SOURCE_NAME).map(PathBuf::as_path)) {
        if !path.exists() {
            errors
                .push(format!("{KEYOS_OVERRIDE_ENV} override for keyos does not exist: {}", path.display()));
        } else {
            println!("using {KEYOS_OVERRIDE_ENV} override for keyos: {}", path.display());
        }
    }

    for (name, submodule) in &config.submodules {
        if let Some(path) = absolute_override(root, overrides.get(name).map(PathBuf::as_path)) {
            if !path.exists() {
                errors.push(format!(
                    "{} override for {} does not exist: {}",
                    submodule.env_override,
                    name,
                    path.display()
                ));
                continue;
            }

            println!("using {} override for {}: {}", submodule.env_override, name, path.display());
            continue;
        }

        if let Some(contents) = contents.as_deref() {
            if !contents.contains(&format!("path = {}", submodule.path)) {
                errors.push(format!(
                    ".gitmodules does not contain path = {} for submodule {}",
                    submodule.path, name
                ));
                continue;
            }
            if !contents.contains(&format!("url = {}", submodule.repo)) {
                errors.push(format!(
                    ".gitmodules does not contain url = {} for submodule {}",
                    submodule.repo, name
                ));
                continue;
            }
        }

        let submodule_dir = root.join(&submodule.path);
        if !submodule_dir.exists() {
            errors.push(format!(
                "source path for {} does not exist: {} (set {} or run nix develop)",
                name,
                submodule_dir.display(),
                submodule.env_override
            ));
            continue;
        }

        let gitlink = util::capture_command(
            Command::new("git").arg("ls-files").arg("--stage").arg("--").arg(&submodule.path),
        )
        .unwrap_or_default();

        if !gitlink.trim().is_empty() {
            let head = util::capture_command(
                Command::new("git").arg("-C").arg(&submodule_dir).arg("rev-parse").arg("HEAD"),
            )?;

            let expected = util::capture_command(
                Command::new("git")
                    .arg("-C")
                    .arg(&submodule_dir)
                    .arg("rev-parse")
                    .arg(format!("{}^{{commit}}", submodule.r#ref)),
            )?;

            if head != expected {
                errors.push(format!(
                    "submodule {} is at {}, expected {} ({})",
                    submodule.path, head, expected, submodule.r#ref
                ));
            }

            continue;
        }

        println!("using local source snapshot for {} at {}", name, submodule_dir.display());
    }

    if !errors.is_empty() {
        return Err(boxed_err(errors.join("\n")));
    }

    Ok(())
}

fn active_keyos_root(root: &Path, explicit_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = absolute_override(root, explicit_override) {
        return Ok(path);
    }

    root.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| boxed_err("sdk workspace root does not have a parent KeyOS source directory"))
}

fn absolute_override(root: &Path, explicit_override: Option<&Path>) -> Option<PathBuf> {
    explicit_override.map(|path| util::absolute_path(root, path))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use super::{check_all, resolve};
    use crate::config::{Config, SubmoduleConfig};

    #[test]
    fn check_all_accepts_override_without_gitmodules() {
        let (_root_guard, root) = temp_dir();
        let override_dir = root.join("vendor").join("slint");
        fs::create_dir_all(&override_dir).unwrap();

        let mut config = Config::default();
        config.submodules.insert(
            "slint".to_string(),
            SubmoduleConfig {
                path: "external/slint".to_string(),
                repo: "git@github.com:Foundation-Devices/slint.git".to_string(),
                r#ref: "v1.12.1-foundation10".to_string(),
                env_override: "SLINT_DIR".to_string(),
            },
        );

        let mut overrides = BTreeMap::new();
        overrides.insert("slint".to_string(), override_dir.clone());

        assert!(check_all(&root, &config, &overrides).is_ok());
    }

    #[test]
    fn check_all_reports_missing_source_when_unresolved() {
        let (_root_guard, root) = temp_dir();

        let mut config = Config::default();
        config.submodules.insert(
            "slint".to_string(),
            SubmoduleConfig {
                path: "external/slint".to_string(),
                repo: "git@github.com:Foundation-Devices/slint.git".to_string(),
                r#ref: "v1.12.1-foundation10".to_string(),
                env_override: "SLINT_DIR".to_string(),
            },
        );

        let error = check_all(&root, &config, &BTreeMap::new()).unwrap_err();
        assert!(error.to_string().contains("set SLINT_DIR or run nix develop"));
    }

    #[test]
    fn keyos_override_resolves_parent_relative_sources() {
        let (_root_guard, root) = temp_dir();
        let (_keyos_guard, keyos_root) = temp_dir();

        let mut overrides = BTreeMap::new();
        overrides.insert("keyos".to_string(), keyos_root.clone());

        let resolver = resolve(&root, &Config::default(), &overrides).unwrap();

        assert_eq!(resolver.keyos_root(), keyos_root.as_path());
        assert_eq!(resolver.resolve_source(&root, "../api/settings"), keyos_root.join("api/settings"));
        assert_eq!(resolver.resolve_source(&root, "crates/cli"), root.join("crates/cli"));
    }

    fn temp_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }
}
