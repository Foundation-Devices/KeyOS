// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{bail, Context};
use clap::Args;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    bootloader::{write_historical_bootloader, HistoricalBootloaderResult},
    builder::project_root,
    BOOTLOADER_IMAGE,
};

const EXTRA_ENTROPY_MARKER: &[u8; 32] = b"extra_entropy_replaced_by_xtask_";
const HISTORICAL_BOOTLOADER_PATH: &str = "target/armv7a-unknown-xous-elf/release/images/boot.bin";
const HISTORICAL_SOURCE_DATE_EPOCH_PATH: &str = "boot/keyos-boot/SOURCE_DATE_EPOCH";
const HOST_UTILITY_PATH: &str = "/usr/bin:/bin";

#[derive(Args)]
pub struct ReproduceBootloaderArgs {
    /// Historical release tag or commit, for example v1.2.0.
    #[arg(value_name = "REF")]
    historical_ref: String,
    /// Number of independent clean builds to compare.
    #[arg(long, default_value_t = 2)]
    builds: usize,
    /// Artifact parent directory.
    #[arg(long, value_name = "DIR")]
    output_root: Option<PathBuf>,
}

#[derive(Serialize)]
struct BuildReport {
    build: usize,
    raw_sha256: String,
    on_device_sha256: String,
    raw_size: usize,
    secure_boot_sram_size: usize,
    marker_sha256: String,
    marker_image: PathBuf,
    normalized_image: PathBuf,
}

#[derive(Serialize)]
struct HostReport {
    system: &'static str,
    machine: &'static str,
}

#[derive(Serialize)]
struct HashToolReport {
    commit: String,
    bootloader_source_sha256: String,
    wrapper_source_sha256: String,
}

#[derive(Serialize)]
struct ReproductionReport<'a> {
    r#ref: &'a str,
    commit: &'a str,
    source_date_epoch: u64,
    host: HostReport,
    hash_tool: HashToolReport,
    builds: &'a [BuildReport],
    reproducible: Option<bool>,
}

struct WorktreeGuard {
    repo: PathBuf,
    path: PathBuf,
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&self.path)
                .current_dir(&self.repo)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub fn run(args: ReproduceBootloaderArgs) -> anyhow::Result<()> {
    anyhow::ensure!(args.builds > 0, "--builds must be at least 1");

    let repo = project_root();
    let commit =
        git_output(&repo, ["rev-parse", "--verify", &format!("{}^{{commit}}", args.historical_ref)])?;
    let source_date_epoch = source_date_epoch_for_commit(&repo, &commit)?;
    let short_commit = &commit[..12];
    let host_system = env::consts::OS;
    let host_machine = env::consts::ARCH;
    if host_system != "linux" || host_machine != "aarch64" {
        println!(
            "WARNING: {host_system} {host_machine} is not the canonical AArch64 Linux build host; \
             the builds can be compared with each other, but their hash may not match a release device."
        );
    }

    let output_root = args
        .output_root
        .map(|path| if path.is_absolute() { path } else { repo.join(path) })
        .unwrap_or_else(|| repo.join("target/bootloader-reproductions"));
    fs::create_dir_all(&output_root)
        .with_context(|| format!("could not create {}", output_root.display()))?;
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let session_dir = output_root.join(format!(
        "{}-{short_commit}-{timestamp}-{}",
        safe_ref_name(&args.historical_ref),
        std::process::id()
    ));
    fs::create_dir(&session_dir).with_context(|| format!("could not create {}", session_dir.display()))?;

    println!("Historical ref:               {}", args.historical_ref);
    println!("Historical commit:            {commit}");
    println!("Historical SOURCE_DATE_EPOCH: {source_date_epoch}");
    println!("Build host:                   {host_system} {host_machine}");
    println!("Artifacts:                    {}", session_dir.display());

    let mut builds = Vec::with_capacity(args.builds);
    for build_number in 1..=args.builds {
        builds.push(build_once(&repo, &commit, &session_dir, build_number)?);
    }

    let first = builds.first().expect("at least one build was requested");
    let builds_match = builds.iter().skip(1).all(|build| {
        build.raw_sha256 == first.raw_sha256 && build.on_device_sha256 == first.on_device_sha256
    });
    let reproducible = (args.builds > 1).then_some(builds_match);
    let report = ReproductionReport {
        r#ref: &args.historical_ref,
        commit: &commit,
        source_date_epoch,
        host: HostReport { system: host_system, machine: host_machine },
        hash_tool: HashToolReport {
            commit: git_output(&repo, ["rev-parse", "HEAD"])?,
            bootloader_source_sha256: sha256_file(&repo.join("xtask/src/bootloader.rs"))?,
            wrapper_source_sha256: sha256_file(&repo.join("xtask/src/reproduce_bootloader.rs"))?,
        },
        builds: &builds,
        reproducible,
    };
    let report_path = session_dir.join("report.json");
    let mut encoded_report = serde_json::to_string_pretty(&report)?;
    encoded_report.push('\n');
    fs::write(&report_path, encoded_report)
        .with_context(|| format!("could not write {}", report_path.display()))?;

    println!();
    println!("Normalized raw SHA256:       {}", first.raw_sha256);
    println!("On-device bootloader SHA256: {}", first.on_device_sha256);
    match reproducible {
        None => println!("Reproducibility:             not tested (one build requested)"),
        Some(true) => {
            println!("Reproducibility:             PASS ({} independent builds match)", args.builds)
        }
        Some(false) => {
            println!("Reproducibility:             FAIL ({} independent builds differ)", args.builds)
        }
    }
    println!("Report:                      {}", report_path.display());

    if reproducible == Some(false) {
        bail!("historical bootloader builds differ");
    }
    Ok(())
}

fn build_once(
    repo: &Path,
    commit: &str,
    session_dir: &Path,
    build_number: usize,
) -> anyhow::Result<BuildReport> {
    let temporary_root = tempfile::Builder::new()
        .prefix(&format!("keyos-bootloader-{build_number}-"))
        .tempdir()
        .context("could not create temporary worktree parent")?;
    let worktree = temporary_root.path().join("source");

    let mut add_worktree = Command::new("git");
    add_worktree.args(["worktree", "add", "--detach"]).arg(&worktree).arg(commit).current_dir(repo);
    run_command(&mut add_worktree)?;
    let _worktree_guard = WorktreeGuard { repo: repo.to_path_buf(), path: worktree.clone() };

    let mut build = Command::new(executable_path("nix")?);
    // Do not let a surrounding `nix develop` append its stdenv flags to the historical shell.
    // HOME is kept only so Nix and Cargo can reuse their download caches. The controlled host PATH
    // supplies legacy undeclared utilities such as `which` without exposing the outer Nix toolchain.
    build
        .args([
            "develop",
            "--ignore-environment",
            "--keep",
            "HOME",
            "--keep",
            "PATH",
            ".#build",
            "--command",
            "cargo",
            "xtask",
            "build-bootloader",
            "--production-bootloader",
            "--extra-entropy",
            &hex::encode(EXTRA_ENTROPY_MARKER),
        ])
        .env("PATH", HOST_UTILITY_PATH)
        .current_dir(&worktree);
    run_command(&mut build)?;

    let historical_image = worktree.join(HISTORICAL_BOOTLOADER_PATH);
    anyhow::ensure!(
        historical_image.is_file(),
        "historical build did not produce {}",
        historical_image.display()
    );

    let output_dir = session_dir.join(format!("build-{build_number}"));
    fs::create_dir(&output_dir).with_context(|| format!("could not create {}", output_dir.display()))?;
    let marker_image = output_dir.join("boot.marker.bin");
    fs::copy(&historical_image, &marker_image).with_context(|| {
        format!("could not copy {} to {}", historical_image.display(), marker_image.display())
    })?;
    let HistoricalBootloaderResult { raw_sha256, on_device_sha256, raw_size, secure_boot_sram_size } =
        write_historical_bootloader(&marker_image, &output_dir)?;
    let normalized_image = output_dir.join(BOOTLOADER_IMAGE);

    println!("Historical bootloader:       {}", marker_image.display());
    println!("Normalized bootloader:       {}", normalized_image.display());
    println!("Raw bootloader size:         {raw_size}");
    println!("Secure Boot SRAM size:       {secure_boot_sram_size}");
    println!("Raw bootloader SHA256:       {raw_sha256}");
    println!("On-device bootloader SHA256: {on_device_sha256}");

    Ok(BuildReport {
        build: build_number,
        raw_sha256,
        on_device_sha256,
        raw_size,
        secure_boot_sram_size,
        marker_sha256: sha256_file(&marker_image)?,
        marker_image,
        normalized_image,
    })
}

fn run_command(command: &mut Command) -> anyhow::Result<()> {
    println!("+ {command:?}");
    let status = command.status().with_context(|| format!("could not run {command:?}"))?;
    anyhow::ensure!(status.success(), "command failed with {status}: {command:?}");
    Ok(())
}

fn executable_path(name: &str) -> anyhow::Result<PathBuf> {
    let path = env::var_os("PATH").context("PATH is not set")?;
    env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .with_context(|| format!("could not find {name} in PATH"))
}

fn git_output<'a>(repo: &Path, args: impl IntoIterator<Item = &'a str>) -> anyhow::Result<String> {
    let output = Command::new("git").args(args).current_dir(repo).output().context("could not run git")?;
    anyhow::ensure!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout).context("git output is not UTF-8").map(|output| output.trim().to_owned())
}

fn source_date_epoch_for_commit(repo: &Path, commit: &str) -> anyhow::Result<u64> {
    let listing = Command::new("git")
        .args(["ls-tree", "--name-only", commit, "--", HISTORICAL_SOURCE_DATE_EPOCH_PATH])
        .current_dir(repo)
        .output()
        .context("could not run git ls-tree")?;
    anyhow::ensure!(
        listing.status.success(),
        "git ls-tree failed: {}",
        String::from_utf8_lossy(&listing.stderr).trim()
    );

    let tracked_epoch = if listing.stdout.is_empty() {
        None
    } else {
        let object = format!("{commit}:{HISTORICAL_SOURCE_DATE_EPOCH_PATH}");
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
        Some(output.stdout)
    };
    let commit_timestamp = git_output(repo, ["show", "-s", "--format=%ct", commit])?;
    select_source_date_epoch(tracked_epoch.as_deref(), &commit_timestamp)
}

fn select_source_date_epoch(tracked_epoch: Option<&[u8]>, commit_timestamp: &str) -> anyhow::Result<u64> {
    let (value, description) = match tracked_epoch {
        Some(bytes) => (
            std::str::from_utf8(bytes).context("historical SOURCE_DATE_EPOCH is not UTF-8")?,
            HISTORICAL_SOURCE_DATE_EPOCH_PATH,
        ),
        None => (commit_timestamp, "historical commit timestamp"),
    };
    value.trim().parse::<u64>().with_context(|| format!("{description} is not an unsigned integer"))
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn safe_ref_name(reference: &str) -> String {
    let mut name = String::with_capacity(reference.len());
    let mut previous_was_separator = false;
    for character in reference.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            name.push(character);
            previous_was_separator = false;
        } else if !previous_was_separator {
            name.push('-');
            previous_was_separator = true;
        }
    }
    let name = name.trim_matches(['-', '.']);
    if name.is_empty() {
        "commit".to_owned()
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_name_is_safe_for_artifact_directory() {
        assert_eq!(safe_ref_name("release/v1.3.0^{commit}"), "release-v1.3.0-commit");
        assert_eq!(safe_ref_name("..."), "commit");
    }

    #[test]
    fn tracked_source_date_epoch_takes_precedence_over_commit_timestamp() {
        assert_eq!(select_source_date_epoch(Some(b"456\n"), "123").unwrap(), 456);
        assert_eq!(select_source_date_epoch(None, "123").unwrap(), 123);
    }

    #[test]
    fn malformed_tracked_source_date_epoch_does_not_fall_back() {
        assert!(select_source_date_epoch(Some(b"not-an-epoch\n"), "123").is_err());
    }
}
