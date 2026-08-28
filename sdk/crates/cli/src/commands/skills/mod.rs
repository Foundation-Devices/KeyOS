// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! `foundation skills` - install the SDK's agent skills into an app repository.
//!
//! The SDK ships its skills at the bundle root, under `.agents/skills/` and
//! `.claude/skills/`. Agent tools resolve skills from the directory they are
//! started in, not from the SDK, so from an app checkout those files are
//! unreachable. Copying them into the app is what makes them usable.
//!
//! The copies are ordinary files the app commits; re-running the command after
//! an SDK upgrade refreshes them.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use foundation_core::SdkRoot;

/// Where agent tools look for project skills.
const SKILL_DIRS: [&str; 2] = [".claude/skills", ".agents/skills"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    Updated,
    Unchanged,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Installed => "installed",
            Outcome::Updated => "updated",
            Outcome::Unchanged => "unchanged",
        }
    }
}

/// Execute the `foundation skills` command.
pub fn execute() -> Result<()> {
    let sdk = SdkRoot::discover().context(
        "Could not locate the Foundation SDK root. Run this command from an SDK development shell, or set FOUNDATION_SDK_ROOT.",
    )?;
    let target = std::env::current_dir().context("Could not determine the current directory")?;

    let skills = install_skills(&sdk, &target)?;
    if skills.is_empty() {
        bail!("The Foundation SDK at {} ships no agent skills", sdk.root().display());
    }

    println!("Installing agent skills from {}", sdk.root().display());
    for (name, outcome) in &skills {
        println!("  {:<10} {name}", outcome.label());
    }
    println!();
    println!("{} skills are now in {}.", skills.len(), SKILL_DIRS.join(" and "));
    println!("Commit them with the app, and re-run 'foundation skills' after an SDK upgrade.");

    Ok(())
}

/// Copy every skill the SDK ships into the agent skill directories under
/// `target`, reporting what each copy did. Skills the SDK does not ship are left
/// in place, so an app's own skills survive.
pub fn install_skills(sdk: &SdkRoot, target: &Path) -> Result<Vec<(String, Outcome)>> {
    let source_root = sdk.skills_path();
    let mut installed = Vec::new();

    for name in skill_names(&source_root)? {
        let source = read_tree(&source_root.join(&name))?;
        let mut outcome = Outcome::Unchanged;
        for skill_dir in SKILL_DIRS {
            let destination = target.join(skill_dir).join(&name);
            outcome = stronger(outcome, sync_tree(&source, &destination)?);
        }
        installed.push((name, outcome));
    }

    Ok(installed)
}

/// Skill directory names, sorted, or an empty list when the SDK ships none.
fn skill_names(source_root: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(source_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("Failed to read {}", source_root.display())),
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to read {}", source_root.display()))?;
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();

    Ok(names)
}

/// Write `source` to `destination`, replacing whatever is there. Rewriting the
/// whole directory drops files a newer SDK removed from the skill.
fn sync_tree(source: &BTreeMap<PathBuf, Vec<u8>>, destination: &Path) -> Result<Outcome> {
    let current = read_tree(destination)?;
    if &current == source {
        return Ok(Outcome::Unchanged);
    }

    if destination.exists() {
        fs::remove_dir_all(destination)
            .with_context(|| format!("Failed to replace {}", destination.display()))?;
    }

    for (relative, contents) in source {
        let path = destination.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        fs::write(&path, contents).with_context(|| format!("Failed to write {}", path.display()))?;
    }

    Ok(if current.is_empty() { Outcome::Installed } else { Outcome::Updated })
}

/// Every file under `dir` keyed by its path relative to `dir`. A missing
/// directory reads as empty.
fn read_tree(dir: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut tree = BTreeMap::new();
    collect_tree(dir, Path::new(""), &mut tree)?;
    Ok(tree)
}

fn collect_tree(dir: &Path, prefix: &Path, tree: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("Failed to read {}", dir.display())),
    };

    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to read {}", dir.display()))?;
        let path = entry.path();
        let relative = prefix.join(entry.file_name());
        if path.is_dir() {
            collect_tree(&path, &relative, tree)?;
        } else {
            let contents = fs::read(&path).with_context(|| format!("Failed to read {}", path.display()))?;
            tree.insert(relative, contents);
        }
    }

    Ok(())
}

fn stronger(left: Outcome, right: Outcome) -> Outcome {
    match (left, right) {
        (Outcome::Installed, _) | (_, Outcome::Installed) => Outcome::Installed,
        (Outcome::Updated, _) | (_, Outcome::Updated) => Outcome::Updated,
        _ => Outcome::Unchanged,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use foundation_core::SdkRoot;

    use super::{install_skills, Outcome, SKILL_DIRS};
    use crate::test_support::make_temp_dir;

    #[test]
    fn install_skills_installs_refreshes_and_keeps_app_skills() {
        let sdk_dir = make_temp_dir("skills-sdk");
        let sdk_root = sdk_dir.path();
        fs::write(sdk_root.join("flake.nix"), "{}").unwrap();
        fs::create_dir_all(sdk_root.join("bin")).unwrap();
        fs::create_dir_all(sdk_root.join("lib").join("keyos")).unwrap();
        let skill_dir = sdk_root.join(".agents").join("skills").join("foundation-cli");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "first").unwrap();
        fs::write(skill_dir.join("references").join("commands.md"), "reference").unwrap();
        let sdk = SdkRoot::from_root(sdk_root.to_path_buf()).unwrap();

        let app_dir = make_temp_dir("skills-app");
        let app = app_dir.path();
        let app_skill = app.join(".claude/skills").join("app-own-skill");
        fs::create_dir_all(&app_skill).unwrap();
        fs::write(app_skill.join("SKILL.md"), "app").unwrap();

        let installed = install_skills(&sdk, app).unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].1, Outcome::Installed);
        for skill_dir in SKILL_DIRS {
            let destination = app.join(skill_dir).join("foundation-cli");
            assert_eq!(fs::read_to_string(destination.join("SKILL.md")).unwrap(), "first");
            assert_eq!(
                fs::read_to_string(destination.join("references").join("commands.md")).unwrap(),
                "reference"
            );
        }

        assert_eq!(install_skills(&sdk, app).unwrap()[0].1, Outcome::Unchanged);

        fs::write(skill_dir.join("SKILL.md"), "second").unwrap();
        fs::remove_file(skill_dir.join("references").join("commands.md")).unwrap();
        assert_eq!(install_skills(&sdk, app).unwrap()[0].1, Outcome::Updated);
        for skill_dir in SKILL_DIRS {
            let destination = app.join(skill_dir).join("foundation-cli");
            assert_eq!(fs::read_to_string(destination.join("SKILL.md")).unwrap(), "second");
            assert!(!destination.join("references").join("commands.md").exists());
        }

        assert_eq!(fs::read_to_string(app_skill.join("SKILL.md")).unwrap(), "app");
    }
}
