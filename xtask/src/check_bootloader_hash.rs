// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{anyhow, bail, Context};
use clap::Args;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    bootloader::recorded_on_device_bootloader_hash, builder::project_root, BOOTLOADER_IMAGE,
    TARGET_TRIPLE_KEYOS,
};

const EXPECTED_FILE: &str = ".github/actions/check-bootloader-hash/expected.toml";
const VERSION_FILE: &str = "boot/keyos-boot/Cargo.toml";
const SOURCE_DATE_EPOCH_FILE: &str = "boot/keyos-boot/SOURCE_DATE_EPOCH";
const FIXED_SOURCE_DATE_EPOCH: &str = "1";
const BOOTSTRAP_VERSION: &str = "0.2.1";
const BOOTSTRAP_NORMALIZED_SHA256: &str = "6d5c7ed481d8a0dcfe6a4ef3f3fe2c207778156f833d9d677a0b49cd4dbb7297";
// The legacy record came from the x86_64 builder; the canonical job now runs on AArch64. Accept only
// this exact host-migration pair. It cannot match once the base record has canonical hashes.
const CANONICAL_HOST_MIGRATION_VERSION: &str = "0.2.5";
const X86_64_NORMALIZED_SHA256: &str = "6d26a2cd327dbdfe982f3e2e3c0d9517b227645d65117360720cf01e48b1d89d";
const AARCH64_NORMALIZED_SHA256: &str = "407d6a41a68bcef1517dd82a08850f2ebe37612eaf852e3ba76ee2268a8d3f8d";

#[derive(Args)]
pub struct CheckBootloaderHashArgs {
    /// Commit used as the bootloader policy baseline.
    #[arg(long, value_name = "COMMIT")]
    base_ref: String,
}

#[derive(Clone, Deserialize)]
struct ExpectedHashes {
    version: String,
    normalized_sha256: String,
    canonical_sha256: Option<String>,
    on_device_sha256: Option<String>,
}

impl ExpectedHashes {
    fn bootstrap() -> Self {
        Self {
            version: BOOTSTRAP_VERSION.to_owned(),
            normalized_sha256: BOOTSTRAP_NORMALIZED_SHA256.to_owned(),
            canonical_sha256: None,
            on_device_sha256: None,
        }
    }

    fn has_canonical_hashes(&self) -> bool {
        self.canonical_sha256.is_some() && self.on_device_sha256.is_some()
    }
}

pub fn run(args: CheckBootloaderHashArgs) -> anyhow::Result<()> {
    let repo = project_root();
    verify_commit(&repo, &args.base_ref)?;

    let current = load_expected(&fs::read(repo.join(EXPECTED_FILE))?, true)?;
    let base = load_base_expected(&repo, &args.base_ref)?;
    let current_source_date_epoch = load_source_date_epoch(&fs::read(repo.join(SOURCE_DATE_EPOCH_FILE))?)?;
    let base_source_date_epoch = load_base_source_date_epoch(&repo, &args.base_ref)?;
    let actual_version = package_version(&repo)?;

    // Fail the cheapest policy check before performing two complete bootloader builds.
    if actual_version != current.version {
        return policy_error(
            format!(
                "The keyos-boot package version ({actual_version}) does not match the tracked version ({}). \
                 Update {EXPECTED_FILE}.",
                current.version
            ),
            VERSION_FILE,
        );
    }

    build_bootloader(&repo, Some(FIXED_SOURCE_DATE_EPOCH))?;
    let images_path = images_path(&repo);
    let normalized_bytes = fs::read(images_path.join(BOOTLOADER_IMAGE))?;
    let normalized_sha256 = sha256(&normalized_bytes);

    println!(
        "Tracked bootloader: version {}, normalized SHA-256 {}",
        current.version, current.normalized_sha256
    );
    println!("Built bootloader:   version {actual_version}, normalized SHA-256 {normalized_sha256}");
    println!("Bootloader SOURCE_DATE_EPOCH: {current_source_date_epoch}");

    build_bootloader(&repo, None)?;
    let canonical_bytes = fs::read(images_path.join(BOOTLOADER_IMAGE))?;
    let canonical_sha256 = sha256(&canonical_bytes);
    let on_device_sha256 = hex::encode(recorded_on_device_bootloader_hash(&images_path, &canonical_bytes)?);
    println!("Canonical raw SHA-256:       {canonical_sha256}");
    println!("Canonical on-device SHA-256: {on_device_sha256}");

    let expected_canonical =
        current.canonical_sha256.as_deref().expect("current expected record requires canonical hash");
    let expected_on_device =
        current.on_device_sha256.as_deref().expect("current expected record requires on-device hash");
    if normalized_sha256 != current.normalized_sha256
        || canonical_sha256 != expected_canonical
        || on_device_sha256 != expected_on_device
    {
        print_expected_values(&actual_version, &normalized_sha256, &canonical_sha256, &on_device_sha256);
    }
    write_github_summary(
        &current,
        &actual_version,
        &normalized_sha256,
        current_source_date_epoch,
        &canonical_sha256,
        &on_device_sha256,
    )?;

    if normalized_sha256 != current.normalized_sha256 {
        return policy_error(
            format!(
                "The normalized bootloader SHA-256 does not match its tracked value. Increase the package \
                 version in {VERSION_FILE}, then update {EXPECTED_FILE}."
            ),
            EXPECTED_FILE,
        );
    }
    if canonical_sha256 != expected_canonical {
        return policy_error(
            "The canonical raw bootloader SHA-256 does not match its tracked value.".to_owned(),
            EXPECTED_FILE,
        );
    }
    if on_device_sha256 != expected_on_device {
        return policy_error(
            "The canonical on-device bootloader SHA-256 does not match its tracked value.".to_owned(),
            EXPECTED_FILE,
        );
    }

    let base_has_canonical_hashes = base.has_canonical_hashes();
    let canonical_host_migration = is_canonical_host_migration(&current, &base);
    if canonical_host_migration {
        println!(
            "Accepting the one-time normalized-hash migration from the legacy x86_64 builder to the \
             canonical AArch64 builder."
        );
    } else if !base_has_canonical_hashes {
        println!("The base branch has no canonical bootloader hashes; accepting only the missing fields.");
    }
    let (normalized_hash_changed, canonical_hash_changed) = tracked_hash_changes(&current, &base);
    if normalized_hash_changed || canonical_hash_changed {
        let current_version = Version::parse(&current.version)
            .with_context(|| format!("invalid tracked bootloader version: {}", current.version))?;
        let base_version = Version::parse(&base.version)
            .with_context(|| format!("invalid base bootloader version: {}", base.version))?;
        if current_version.cmp_precedence(&base_version) != std::cmp::Ordering::Greater {
            return policy_error(
                format!(
                    "The tracked bootloader hash changed, but its version did not increase (base: {}, merge \
                     result: {}).",
                    base.version, current.version
                ),
                EXPECTED_FILE,
            );
        }
        if let Some(base_epoch) = base_source_date_epoch {
            if current_source_date_epoch <= base_epoch {
                return policy_error(
                    format!(
                        "The tracked bootloader hash changed, but {SOURCE_DATE_EPOCH_FILE} did not increase \
                         (base: {base_epoch}, merge result: {current_source_date_epoch})."
                    ),
                    SOURCE_DATE_EPOCH_FILE,
                );
            }
        }
    } else if let Some(base_epoch) = base_source_date_epoch {
        if current_source_date_epoch != base_epoch {
            return policy_error(
                format!(
                    "{SOURCE_DATE_EPOCH_FILE} changed without a normalized bootloader hash change. Keep the \
                     epoch stable for an unchanged bootloader."
                ),
                SOURCE_DATE_EPOCH_FILE,
            );
        }
    }

    println!("The built bootloader matches its tracked version and hash.");
    Ok(())
}

fn load_expected(bytes: &[u8], require_canonical: bool) -> anyhow::Result<ExpectedHashes> {
    let text = std::str::from_utf8(bytes).context("bootloader hash record is not UTF-8")?;
    let record: ExpectedHashes = toml::from_str(text).context("invalid bootloader hash record")?;
    Version::parse(&record.version).with_context(|| format!("invalid SemVer version: {}", record.version))?;
    validate_hash("normalized_sha256", &record.normalized_sha256)?;
    for (name, value) in [
        ("canonical_sha256", record.canonical_sha256.as_deref()),
        ("on_device_sha256", record.on_device_sha256.as_deref()),
    ] {
        if let Some(value) = value {
            validate_hash(name, value)?;
        } else if require_canonical {
            bail!("the tracked bootloader record must contain {name}");
        }
    }
    Ok(record)
}

fn tracked_hash_changes(current: &ExpectedHashes, base: &ExpectedHashes) -> (bool, bool) {
    (
        current.normalized_sha256 != base.normalized_sha256 && !is_canonical_host_migration(current, base),
        base.has_canonical_hashes() && current.canonical_sha256 != base.canonical_sha256,
    )
}

fn is_canonical_host_migration(current: &ExpectedHashes, base: &ExpectedHashes) -> bool {
    !base.has_canonical_hashes()
        && base.version == CANONICAL_HOST_MIGRATION_VERSION
        && current.version == CANONICAL_HOST_MIGRATION_VERSION
        && base.normalized_sha256 == X86_64_NORMALIZED_SHA256
        && current.normalized_sha256 == AARCH64_NORMALIZED_SHA256
}

fn validate_hash(name: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "the tracked bootloader {name} must be 64 lowercase hexadecimal characters"
    );
    Ok(())
}

fn load_source_date_epoch(bytes: &[u8]) -> anyhow::Result<u64> {
    let value = std::str::from_utf8(bytes)?.trim();
    anyhow::ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "{SOURCE_DATE_EPOCH_FILE} must contain one unsigned decimal integer"
    );
    let epoch = value.parse::<u64>()?;
    anyhow::ensure!(epoch > 0, "{SOURCE_DATE_EPOCH_FILE} must be greater than zero");
    Ok(epoch)
}

fn verify_commit(repo: &Path, base_ref: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["cat-file", "-e", &format!("{base_ref}^{{commit}}")])
        .current_dir(repo)
        .status()?;
    anyhow::ensure!(status.success(), "the bootloader base ref is not a commit: {base_ref}");
    Ok(())
}

fn load_base_expected(repo: &Path, base_ref: &str) -> anyhow::Result<ExpectedHashes> {
    match git_show_file(repo, base_ref, EXPECTED_FILE)? {
        Some(bytes) => load_expected(&bytes, false),
        None => {
            println!("The base branch has no {EXPECTED_FILE}; using the bootstrap record.");
            Ok(ExpectedHashes::bootstrap())
        }
    }
}

fn load_base_source_date_epoch(repo: &Path, base_ref: &str) -> anyhow::Result<Option<u64>> {
    git_show_file(repo, base_ref, SOURCE_DATE_EPOCH_FILE)?
        .map(|bytes| load_source_date_epoch(&bytes))
        .transpose()
}

fn git_show_file(repo: &Path, commit: &str, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let listing = Command::new("git")
        .args(["ls-tree", "--name-only", commit, "--", path])
        .current_dir(repo)
        .output()
        .context("could not run git ls-tree")?;
    anyhow::ensure!(
        listing.status.success(),
        "git ls-tree failed: {}",
        String::from_utf8_lossy(&listing.stderr).trim()
    );
    if listing.stdout.is_empty() {
        return Ok(None);
    }

    let object = format!("{commit}:{path}");
    let output = Command::new("git")
        .args(["show", &object])
        .current_dir(repo)
        .output()
        .context("could not run git show")?;
    anyhow::ensure!(
        output.status.success(),
        "git show failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(Some(output.stdout))
}

fn package_version(repo: &Path) -> anyhow::Result<String> {
    let metadata = cargo_metadata::MetadataCommand::new().current_dir(repo).no_deps().exec()?;
    let versions: Vec<String> = metadata
        .packages
        .iter()
        .filter(|package| package.name == "keyos-boot")
        .map(|package| package.version.to_string())
        .collect();
    anyhow::ensure!(versions.len() == 1, "expected one keyos-boot package, found {}", versions.len());
    Ok(versions[0].clone())
}

fn build_bootloader(repo: &Path, source_date_epoch: Option<&str>) -> anyhow::Result<()> {
    let mut command = Command::new("cargo");
    command
        .args(["xtask", "build-bootloader", "--production-bootloader"])
        .current_dir(repo)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match source_date_epoch {
        Some(epoch) => {
            command.env("KEYOS_SOURCE_DATE_EPOCH", epoch);
        }
        None => {
            command.env_remove("KEYOS_SOURCE_DATE_EPOCH");
        }
    }
    let status = command.status().context("could not build bootloader")?;
    anyhow::ensure!(status.success(), "bootloader build failed with {status}");
    Ok(())
}

fn images_path(repo: &Path) -> PathBuf {
    repo.join("target").join(TARGET_TRIPLE_KEYOS).join("release").join("images")
}

fn sha256(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

fn print_expected_values(version: &str, normalized: &str, canonical: &str, on_device: &str) {
    eprintln!(
        "After confirming this build ran on AArch64 Linux in the pinned Nix environment, paste these \
         values into {EXPECTED_FILE}:\n\nversion = \"{version}\"\nnormalized_sha256 = \
         \"{normalized}\"\ncanonical_sha256 = \"{canonical}\"\non_device_sha256 = \"{on_device}\""
    );
}

fn write_github_summary(
    current: &ExpectedHashes,
    actual_version: &str,
    normalized: &str,
    source_date_epoch: u64,
    canonical: &str,
    on_device: &str,
) -> anyhow::Result<()> {
    let Some(summary_path) = env::var_os("GITHUB_STEP_SUMMARY") else {
        return Ok(());
    };
    let mut summary = fs::OpenOptions::new().create(true).append(true).open(summary_path)?;
    writeln!(summary, "### Bootloader hash check\n")?;
    writeln!(summary, "| Source | Version | Normalized SHA-256 |")?;
    writeln!(summary, "| --- | --- | --- |")?;
    writeln!(summary, "| Tracked | `{}` | `{}` |", current.version, current.normalized_sha256)?;
    writeln!(summary, "| Built | `{actual_version}` | `{normalized}` |")?;
    writeln!(summary, "\nCanonical `SOURCE_DATE_EPOCH`: `{source_date_epoch}`")?;
    writeln!(summary, "\nCanonical raw SHA-256: `{canonical}`")?;
    writeln!(summary, "\nCanonical on-device SHA-256: `{on_device}`")?;
    Ok(())
}

fn policy_error<T>(message: String, file: &str) -> anyhow::Result<T> {
    println!("::error file={file},title=Bootloader hash check failed::{message}");
    Err(anyhow!(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn complete_expected_record_is_valid() {
        let record = format!(
            "version = \"1.2.3\"\nnormalized_sha256 = \"{HASH}\"\ncanonical_sha256 = \
             \"{HASH}\"\non_device_sha256 = \"{HASH}\"\n"
        );
        let parsed = load_expected(record.as_bytes(), true).unwrap();
        assert_eq!(parsed.version, "1.2.3");
        assert!(parsed.has_canonical_hashes());
    }

    #[test]
    fn legacy_base_record_may_omit_canonical_hashes() {
        let record = format!("version = \"1.2.3\"\nnormalized_sha256 = \"{HASH}\"\n");
        assert!(!load_expected(record.as_bytes(), false).unwrap().has_canonical_hashes());
        assert!(load_expected(record.as_bytes(), true).is_err());
    }

    #[test]
    fn hash_record_rejects_non_lowercase_hex() {
        let record = format!("version = \"1.2.3\"\nnormalized_sha256 = \"{}\"\n", HASH.to_uppercase());
        assert!(load_expected(record.as_bytes(), false).is_err());
    }

    #[test]
    fn source_date_epoch_must_be_positive_decimal() {
        assert_eq!(load_source_date_epoch(b"123\n").unwrap(), 123);
        assert!(load_source_date_epoch(b"0\n").is_err());
        assert!(load_source_date_epoch(b"-1\n").is_err());
    }

    #[test]
    fn semver_build_metadata_does_not_increase_precedence() {
        let base = Version::parse("1.2.3+build1").unwrap();
        let current = Version::parse("1.2.3+build2").unwrap();
        assert_eq!(current.cmp_precedence(&base), std::cmp::Ordering::Equal);
    }

    #[test]
    fn legacy_base_still_detects_normalized_hash_changes() {
        let current = ExpectedHashes {
            version: "1.2.3".to_owned(),
            normalized_sha256: HASH.to_owned(),
            canonical_sha256: Some(HASH.to_owned()),
            on_device_sha256: Some(HASH.to_owned()),
        };
        let base = ExpectedHashes {
            version: "1.2.2".to_owned(),
            normalized_sha256: "1".repeat(64),
            canonical_sha256: None,
            on_device_sha256: None,
        };
        assert_eq!(tracked_hash_changes(&current, &base), (true, false));
    }

    #[test]
    fn canonical_host_migration_exception_matches_only_the_known_hash_pair() {
        let current = ExpectedHashes {
            version: CANONICAL_HOST_MIGRATION_VERSION.to_owned(),
            normalized_sha256: AARCH64_NORMALIZED_SHA256.to_owned(),
            canonical_sha256: Some(HASH.to_owned()),
            on_device_sha256: Some(HASH.to_owned()),
        };
        let base = ExpectedHashes {
            version: CANONICAL_HOST_MIGRATION_VERSION.to_owned(),
            normalized_sha256: X86_64_NORMALIZED_SHA256.to_owned(),
            canonical_sha256: None,
            on_device_sha256: None,
        };
        assert!(is_canonical_host_migration(&current, &base));
        assert_eq!(tracked_hash_changes(&current, &base), (false, false));

        let mut unrelated_current = current.clone();
        unrelated_current.normalized_sha256 = "1".repeat(64);
        assert!(!is_canonical_host_migration(&unrelated_current, &base));
        assert_eq!(tracked_hash_changes(&unrelated_current, &base), (true, false));
    }

    #[test]
    fn git_show_file_distinguishes_a_missing_path_from_a_git_error() {
        let repo = project_root();
        assert!(git_show_file(&repo, "HEAD", "this/path/does/not/exist").unwrap().is_none());
        assert!(git_show_file(&repo, "this-commit-does-not-exist", EXPECTED_FILE).is_err());
    }
}
