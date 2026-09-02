// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{
    boxed_err, selected_targets, CompileEntry, Config, CopyBundle, CopyEntry, CopyFilter, Result,
};
use crate::package::{self, PackageArgs};
use crate::submodules::{self, ResolvedFrom, SourceOverrides, SourceResolver};
use crate::util;

const SDK_USER_FLAKE_SOURCE: &str = "nix/sdk-user-flake.nix";
const SDK_USER_FLAKE_LOCK_SOURCE: &str = "nix/sdk-user-flake.lock";
const KEYOS_HOSTED_KERNEL_PACKAGE: &str = "keyos-kernel";
const KEYOS_HOSTED_RUNTIME_ROOT: &str = "lib/keyos/simulator";
const SOURCE_UI_COMPONENTS_DIR: &str = "ui2/components/ui";
const SOURCE_UI_RESOURCES_DIR: &str = "ui2/resources";
const STAGED_SDK_UI_ROOT: &str = "ui/ui";
const STAGED_KEYOS_SDK_UI_ROOT: &str = "lib/keyos/ui/ui";
const STAGED_SDK_RESOURCES_ROOT: &str = "resources";
const BT_PLACEHOLDER_DESTINATION: &str = "lib/keyos/api/bt";
const BT_PLACEHOLDER_MANIFEST: &str = include_str!("../assets/bt-placeholder/Cargo.toml.template");
const BT_PLACEHOLDER_LIB: &str = include_str!("../assets/bt-placeholder/lib.rs");
const DOCS_BUNDLE_LOCK_PATH: &str = "target/docs-api.lock";
const DOCS_BUNDLE_OUTPUT_PATH: &str = "target/sdk-docs/api";
const DOCS_BUNDLE_LOCK_HELD_ENV: &str = "KEYOS_DOCS_BUNDLE_LOCK_HELD";
const KEYOS_LEGACY_UI_SOURCE_ROOT: &str = "ui/ui";
const KEYOS_LEGACY_UI_ASSET_DIRS: &[&str] = &["icons", "images", "fonts"];
const SLINT_SDK_SEED_DIRS: &[&str] = &[
    "api/rs/build",
    "api/rs/macros",
    "api/rs/slint",
    "helper_crates/const-field-offset",
    "helper_crates/vtable",
    "internal/backends/linuxkms",
    "internal/backends/qt",
    "internal/backends/selector",
    "internal/backends/winit",
    "internal/common",
    "internal/compiler",
    "internal/core",
    "internal/core-macros",
    "internal/interpreter",
    "internal/renderers/femtovg",
    "internal/renderers/skia",
];
const SLINT_SDK_ROOT_FILES: &[&str] = &["Cargo.lock", "LICENSE.md"];
const SLINT_SDK_ROOT_DIRS: &[&str] = &[".cargo", "LICENSES"];
const CARGO_PACKAGE_EXCLUDED_DIR_NAMES: &[&str] = &[".git", "target"];
const SLINT_SDK_EXCLUDED_DIR_NAMES: &[&str] =
    &[".git", "demos", "docs", "editors", "examples", "node_modules", "target", "tests"];

#[derive(Clone, Debug)]
pub struct BuildArgs {
    pub targets: Vec<String>,
    pub release: bool,
    pub package: bool,
    pub skip_simulator: bool,
    pub skip_docs: bool,
    pub source_overrides: SourceOverrides,
    pub sign: bool,
    pub sign_key: Option<String>,
    pub output_dir: PathBuf,
    pub jobs: Option<usize>,
    pub verbose: bool,
}

#[derive(Clone, Debug)]
pub struct CommonBuildArgs {
    pub release: bool,
    pub package: bool,
    pub source_overrides: SourceOverrides,
    pub output_dir: PathBuf,
    pub verbose: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CheckLayoutArgs {
    pub source_overrides: SourceOverrides,
    pub verbose: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SmokeCheckArgs {
    pub source_overrides: SourceOverrides,
    pub sign: bool,
    pub sign_key: Option<String>,
    pub verbose: bool,
}

impl Default for BuildArgs {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            release: false,
            package: false,
            skip_simulator: false,
            skip_docs: false,
            source_overrides: BTreeMap::new(),
            sign: false,
            sign_key: None,
            output_dir: PathBuf::from("dist"),
            jobs: None,
            verbose: false,
        }
    }
}

impl Default for CommonBuildArgs {
    fn default() -> Self {
        Self {
            release: false,
            package: false,
            source_overrides: BTreeMap::new(),
            output_dir: PathBuf::from("dist"),
            verbose: false,
        }
    }
}

impl BuildArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut args = Self::default();
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--target" => args.targets.push(next_value(&mut iter, "--target")?),
                "--release" => args.release = true,
                "--package" => args.package = true,
                "--skip-simulator" => args.skip_simulator = true,
                "--skip-docs" => args.skip_docs = true,
                "--keyos-dir" => {
                    parse_override(&mut args.source_overrides, "keyos", &mut iter, "--keyos-dir")?
                }
                "--slint-dir" => {
                    parse_override(&mut args.source_overrides, "slint", &mut iter, "--slint-dir")?
                }
                "--sign" => args.sign = true,
                "--sign-key" => args.sign_key = Some(next_value(&mut iter, "--sign-key")?),
                "--output-dir" => args.output_dir = PathBuf::from(next_value(&mut iter, "--output-dir")?),
                "--jobs" => {
                    let raw_jobs = next_value(&mut iter, "--jobs")?;
                    args.jobs = Some(
                        raw_jobs
                            .parse()
                            .map_err(|_| boxed_err(format!("invalid --jobs value: {raw_jobs}")))?,
                    );
                }
                "--verbose" => args.verbose = true,
                other => return Err(boxed_err(format!("unsupported build option: {other}"))),
            }
        }

        Ok(args)
    }
}

impl CommonBuildArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut args = Self::default();
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--release" => args.release = true,
                "--package" => args.package = true,
                "--keyos-dir" => {
                    parse_override(&mut args.source_overrides, "keyos", &mut iter, "--keyos-dir")?
                }
                "--slint-dir" => {
                    parse_override(&mut args.source_overrides, "slint", &mut iter, "--slint-dir")?
                }
                "--output-dir" => args.output_dir = PathBuf::from(next_value(&mut iter, "--output-dir")?),
                "--verbose" => args.verbose = true,
                other => return Err(boxed_err(format!("unsupported build-common option: {other}"))),
            }
        }

        Ok(args)
    }
}

impl CheckLayoutArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut args = Self::default();
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--keyos-dir" => {
                    parse_override(&mut args.source_overrides, "keyos", &mut iter, "--keyos-dir")?
                }
                "--slint-dir" => {
                    parse_override(&mut args.source_overrides, "slint", &mut iter, "--slint-dir")?
                }
                "--verbose" => args.verbose = true,
                other => return Err(boxed_err(format!("unsupported check-layout option: {other}"))),
            }
        }

        Ok(args)
    }
}

impl SmokeCheckArgs {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut args = Self::default();
        let mut iter = raw.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--keyos-dir" => {
                    parse_override(&mut args.source_overrides, "keyos", &mut iter, "--keyos-dir")?
                }
                "--slint-dir" => {
                    parse_override(&mut args.source_overrides, "slint", &mut iter, "--slint-dir")?
                }
                "--sign" => args.sign = true,
                "--sign-key" => args.sign_key = Some(next_value(&mut iter, "--sign-key")?),
                "--verbose" => args.verbose = true,
                other => return Err(boxed_err(format!("unsupported smoke-check option: {other}"))),
            }
        }

        Ok(args)
    }
}

#[derive(Default)]
struct LayoutReport {
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyosSlintPin {
    tag: String,
    commit: String,
}

/// RAII lock on the SDK build's `.stage/` directory. Held for the duration of
/// `run` so that two concurrent xtask invocations (parallel CI runs, a user
/// driving xtask in two terminals) can't interleave writes into the same
/// staging tree. Uses `OpenOptions::create_new` for atomicity; the lock file
/// records the PID of the holder so the next operator knows whose run to
/// abort if it ever sticks around after a crash.
#[derive(Debug)]
struct StageDirLock {
    path: PathBuf,
}

impl StageDirLock {
    fn acquire(output_dir: &Path, stage_root: &Path) -> Result<Self> {
        let path = output_dir.join(".stage.lock");
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                writeln!(file, "pid={}", std::process::id()).ok();
                Ok(Self { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = fs::read_to_string(&path).unwrap_or_default();
                Err(boxed_err(format!(
                    "another xtask invocation appears to be using {}; lock file says: {} \
                     (remove {} manually if no xtask process is running)",
                    stage_root.display(),
                    existing.trim(),
                    path.display(),
                )))
            }
            Err(e) => Err(boxed_err(format!("could not create stage lock {}: {e}", path.display()))),
        }
    }
}

impl Drop for StageDirLock {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            // Stage cleanup also removes the lock file, so a missing-file error here
            // is normal. Log anything else so the next run isn't blocked silently.
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("warning: could not remove stage lock {}: {e}", self.path.display());
            }
        }
    }
}

pub fn run(root: &Path, config: &Config, args: &BuildArgs) -> Result<()> {
    let targets = selected_targets(config, &args.targets)?;
    let mut source_overrides = args.source_overrides.clone();
    submodules::apply_env_overrides(config, &mut source_overrides);
    ensure_release_sources_are_pinned(args.release, &args.source_overrides, &source_overrides, config)?;

    let output_dir = util::absolute_path(root, &args.output_dir);
    util::ensure_dir(&output_dir)?;

    // Hold a file-based lock for the entire build so two parallel xtask
    // invocations can't clobber each other's stage directories. The lock lives
    // alongside (not inside) `.stage/` so the imminent `ensure_clean_dir` call
    // doesn't wipe it out from under us. Released when `_stage_lock` drops.
    let stage_root = package::stage_root_dir(&output_dir);
    let _stage_lock = StageDirLock::acquire(&output_dir, &stage_root)?;

    util::ensure_clean_dir(&stage_root)?;
    let resolver = submodules::resolve(root, config, &source_overrides)?;
    ensure_layout(root, config, &resolver, args.verbose)?;

    let host_target = host_triple();

    build_common_stage(root, config, args, &output_dir, &resolver)?;
    for target in &targets {
        build_target_stage(root, config, args, &output_dir, target, &host_target, &resolver)?;
    }

    if args.package || args.sign {
        let sign_key = if args.sign {
            Some(args.sign_key.clone().or_else(|| package::default_sign_key(config)).ok_or_else(|| {
                boxed_err(format!("--sign requires --sign-key or {}", config.signing.key_env))
            })?)
        } else {
            None
        };
        let package_args = PackageArgs {
            targets,
            version: None,
            output_dir: args.output_dir.clone(),
            verbose: args.verbose,
        };
        package::run(root, config, &package_args, sign_key.as_deref(), args.verbose)?;
    }

    Ok(())
}

pub fn run_common(root: &Path, config: &Config, args: &CommonBuildArgs) -> Result<()> {
    let mut source_overrides = args.source_overrides.clone();
    submodules::apply_env_overrides(config, &mut source_overrides);
    ensure_release_sources_are_pinned(args.release, &args.source_overrides, &source_overrides, config)?;

    let output_dir = util::absolute_path(root, &args.output_dir);
    util::ensure_dir(&output_dir)?;
    let stage_root = package::stage_root_dir(&output_dir);
    let _stage_lock = StageDirLock::acquire(&output_dir, &stage_root)?;
    util::ensure_dir(&stage_root)?;

    let resolver = submodules::resolve(root, config, &source_overrides)?;
    ensure_layout(root, config, &resolver, args.verbose)?;
    let build_args = BuildArgs {
        release: args.release,
        output_dir: args.output_dir.clone(),
        verbose: args.verbose,
        ..Default::default()
    };
    build_common_stage(root, config, &build_args, &output_dir, &resolver)?;

    if args.package {
        package::package_common(root, config, &args.output_dir, args.verbose)?;
    }

    Ok(())
}

fn ensure_release_sources_are_pinned(
    release: bool,
    explicit_overrides: &SourceOverrides,
    resolved_overrides: &SourceOverrides,
    config: &Config,
) -> Result<()> {
    if !release {
        return Ok(());
    }
    if !explicit_overrides.is_empty() {
        return Err(boxed_err(
            "release builds do not allow --keyos-dir or --slint-dir; use the sources pinned by the SDK build environment",
        ));
    }

    let local_overrides = resolved_overrides
        .keys()
        .filter(|name| name.as_str() != "slint")
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !local_overrides.is_empty() {
        return Err(boxed_err(format!(
            "release builds do not allow local source overrides for {}; unset KEYOS_DIR/SLINT_DIR and use the sources pinned by the SDK build environment",
            local_overrides.join(", ")
        )));
    }

    let slint_source = resolved_overrides.get("slint").ok_or_else(|| {
        boxed_err("release builds require the immutable Slint source pinned by the SDK build environment; run from nix develop")
    })?;
    let slint_hash = config
        .submodules
        .get("slint")
        .map(|slint| slint.source_hash.as_str())
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| boxed_err("sdk-build.toml is missing submodules.slint.source_hash"))?;
    validate_release_slint_source(slint_source, slint_hash)?;
    Ok(())
}

fn validate_release_slint_source(source: &Path, expected_hash: &str) -> Result<()> {
    let canonical = fs::canonicalize(source).map_err(|error| {
        boxed_err(format!("failed to resolve release Slint source {}: {error}", source.display()))
    })?;
    if !canonical.starts_with("/nix/store") {
        return Err(boxed_err(format!(
            "release Slint source {} is mutable; use the source pinned by nix develop",
            canonical.display()
        )));
    }

    let actual_hash = util::capture_command(Command::new("nix").arg("hash").arg("path").arg(&canonical))
        .map_err(|error| {
            boxed_err(format!("failed to hash release Slint source {}: {error}", canonical.display()))
        })?;
    validate_release_slint_source_hash(&canonical, expected_hash, &actual_hash)
}

fn validate_release_slint_source_hash(source: &Path, expected_hash: &str, actual_hash: &str) -> Result<()> {
    if actual_hash == expected_hash {
        return Ok(());
    }

    Err(boxed_err(format!(
        "release Slint source {} has content hash {}, expected {} from sdk-build.toml",
        source.display(),
        actual_hash,
        expected_hash
    )))
}

pub fn check_layout(root: &Path, config: &Config, args: &CheckLayoutArgs) -> Result<()> {
    let mut source_overrides = args.source_overrides.clone();
    submodules::apply_env_overrides(config, &mut source_overrides);
    submodules::check_all(root, config, &source_overrides)?;
    let resolver = submodules::resolve(root, config, &source_overrides)?;
    ensure_layout(root, config, &resolver, args.verbose)?;
    println!("layout OK");
    Ok(())
}

pub fn smoke_check(root: &Path, config: &Config, args: &SmokeCheckArgs) -> Result<()> {
    let mut source_overrides = args.source_overrides.clone();
    submodules::apply_env_overrides(config, &mut source_overrides);
    submodules::check_all(root, config, &source_overrides)?;
    let resolver = submodules::resolve(root, config, &source_overrides)?;
    ensure_layout(root, config, &resolver, args.verbose)?;

    let resolved_sign_key =
        if args.sign {
            Some(args.sign_key.clone().or_else(|| package::default_sign_key(config)).ok_or_else(|| {
                boxed_err(format!("--sign requires --sign-key or {}", config.signing.key_env))
            })?)
        } else {
            None
        };

    package::check_prerequisites(resolved_sign_key.as_deref())?;
    println!("smoke checks OK");
    Ok(())
}

fn build_common_stage(
    root: &Path,
    config: &Config,
    args: &BuildArgs,
    output_dir: &Path,
    resolver: &SourceResolver,
) -> Result<()> {
    let stage_dir = package::common_stage_dir(output_dir);
    let copy_entries = copy_entries_for_bundle(config, CopyBundle::Common);
    util::ensure_clean_dir(&stage_dir)?;
    util::ensure_dir(&stage_dir.join("lib"))?;

    for entry in &copy_entries {
        copy_entry(root, &stage_dir, entry, resolver)?;
    }

    stage_bt_error_placeholder(resolver.keyos_root(), &stage_dir)?;
    stage_keyos_workspace_root(root, config, &stage_dir, &config.expanded_copy_entries(), resolver)?;
    stage_shared_ui_artifact(resolver.keyos_root(), &stage_dir, args.verbose)?;

    if !args.skip_docs {
        build_docs(root, &stage_dir, config, resolver, args.verbose)?;
    }

    util::copy_file(&root.join(SDK_USER_FLAKE_SOURCE), &stage_dir.join("flake.nix"))?;
    util::copy_file_if_exists(&root.join(SDK_USER_FLAKE_LOCK_SOURCE), &stage_dir.join("flake.lock"))?;
    util::copy_file(&root.join("setup.sh"), &stage_dir.join("setup.sh"))?;
    fs::write(stage_dir.join("manifest.toml"), render_common_manifest(root, config, args.release))?;

    verify_common_stage(&stage_dir, args.skip_docs)?;

    Ok(())
}

fn build_target_stage(
    root: &Path,
    config: &Config,
    args: &BuildArgs,
    output_dir: &Path,
    target: &str,
    host_target: &str,
    resolver: &SourceResolver,
) -> Result<()> {
    let stage_dir = package::target_stage_dir(output_dir, target);
    let copy_entries = copy_entries_for_bundle(config, CopyBundle::Target);
    let stage_simulator = should_stage_simulator_for_target(target, args.skip_simulator, host_target);
    util::ensure_clean_dir(&stage_dir)?;
    util::ensure_dir(&stage_dir.join("bin"))?;
    util::ensure_dir(&stage_dir.join("lib"))?;

    for entry in &config.compile {
        if entry.name == "simulator" {
            if !stage_simulator {
                if !args.skip_simulator {
                    eprintln!(
                        "warning: skipping simulator runtime for non-host SDK target {target}; \
                         KeyOS hosted simulator can only be packaged for host target {host_target}"
                    );
                }
                continue;
            }
            stage_simulator_runtime(root, config, &stage_dir, target, entry, args, resolver)?;
            continue;
        }
        compile_entry(root, config, &stage_dir, target, host_target, entry, args, resolver)?;
    }

    for entry in &copy_entries {
        copy_entry(root, &stage_dir, entry, resolver)?;
    }

    let manifest =
        render_manifest(root, &stage_dir, target, config, resolver, args.release, !stage_simulator)?;
    fs::write(stage_dir.join("manifest.toml"), manifest)?;
    verify_target_stage(&stage_dir, !stage_simulator)?;

    Ok(())
}

fn should_stage_simulator_for_target(target: &str, skip_simulator: bool, host_target: &str) -> bool {
    !skip_simulator && target == host_target
}

fn copy_entries_for_bundle(config: &Config, bundle: CopyBundle) -> Vec<CopyEntry> {
    config.expanded_copy_entries().into_iter().filter(|entry| entry.bundle == bundle).collect()
}

/// The SKILL.md of every staged agent skill, under both tool directories.
/// `.claude/skills` is a symlink to `.agents/skills` in the source tree, so a
/// copy that did not follow it leaves the directory Claude Code reads empty.
fn bundled_skill_paths(stage_dir: &Path) -> Result<Vec<PathBuf>> {
    let staged = stage_dir.join(".agents").join("skills");
    let entries = fs::read_dir(&staged)
        .map_err(|error| boxed_err(format!("could not read {}: {error}", staged.display())))?;

    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        paths.push(entry.path().join("SKILL.md"));
        paths.push(stage_dir.join(".claude").join("skills").join(entry.file_name()).join("SKILL.md"));
    }

    if paths.is_empty() {
        return Err(boxed_err(format!("no agent skills staged at {}", staged.display())));
    }

    Ok(paths)
}

fn verify_common_stage(stage_dir: &Path, skip_docs: bool) -> Result<()> {
    let mut required_paths = vec![
        stage_dir.join("manifest.toml"),
        stage_dir.join("lib").join("keyos").join("Cargo.toml"),
        stage_dir.join(BT_PLACEHOLDER_DESTINATION).join("Cargo.toml"),
        stage_dir.join(BT_PLACEHOLDER_DESTINATION).join("src").join("error.rs"),
        stage_dir.join("ui").join("ui").join("theme.slint"),
        stage_dir
            .join("lib")
            .join("keyos")
            .join("sdk")
            .join("crates")
            .join("foundation-themes")
            .join("Cargo.toml"),
        stage_dir.join("lib").join("keyos").join("ui2").join("components").join("Cargo.toml"),
        stage_dir
            .join("lib")
            .join("keyos")
            .join("sdk")
            .join("crates")
            .join("foundation-themes")
            .join("src")
            .join("build.rs"),
        stage_dir
            .join("lib")
            .join("keyos")
            .join("sdk")
            .join("crates")
            .join("foundation-themes")
            .join("themes")
            .join("base_theme.json"),
        stage_dir.join("lib").join("keyos").join("utils").join("fiat-symbols").join("Cargo.toml"),
        stage_dir.join("lib").join("keyos").join("utils").join("localizer-codegen").join("Cargo.toml"),
        stage_dir.join("resources").join("icons").join("loader.svg"),
        stage_dir.join("flake.nix"),
        stage_dir.join("setup.sh"),
    ];
    required_paths.extend(bundled_skill_paths(stage_dir)?);

    if !skip_docs {
        required_paths.push(stage_dir.join("docs").join("guide"));
        required_paths.push(stage_dir.join("docs").join("api"));
        required_paths.push(stage_dir.join("docs").join("guide").join("src").join("foundation-cli.md"));
    }

    let missing = required_paths.into_iter().filter(|path| !path.exists()).collect::<Vec<_>>();

    if missing.is_empty() {
        return verify_staged_path_dependencies(&stage_dir.join("lib").join("keyos"));
    }

    Err(boxed_err(format!(
        "staged common SDK content at {} is missing required paths:\n{}",
        stage_dir.display(),
        missing.iter().map(|path| format!("- {}", path.display())).collect::<Vec<_>>().join("\n")
    )))
}

/// Every `path = ` a staged manifest names must exist in the bundle. A crate the
/// copy list forgets resolves fine in this checkout and fails for everyone who
/// installs the SDK, so the miss has to fail the build that produced it.
fn verify_staged_path_dependencies(keyos_root: &Path) -> Result<()> {
    let mut missing = Vec::new();
    for manifest in staged_manifests(keyos_root)? {
        let Some(base) = manifest.parent() else { continue };
        let contents = fs::read_to_string(&manifest)?;
        for path in manifest_dependency_paths(&contents) {
            if !base.join(&path).exists() {
                missing.push(format!("{} declares path {path}", manifest.display()));
            }
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    Err(boxed_err(format!(
        "staged manifests name path dependencies the bundle does not carry:\n{}",
        missing.iter().map(|entry| format!("- {entry}")).collect::<Vec<_>>().join("\n")
    )))
}

fn staged_manifests(root: &Path) -> Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "Cargo.toml") {
                manifests.push(path);
            }
        }
    }
    Ok(manifests)
}

/// The value of every `path = "..."` in `manifest`, wherever it sits: a section
/// of its own, an inline table, or a target-specific block.
fn manifest_dependency_paths(manifest: &str) -> Vec<String> {
    manifest
        .lines()
        .map(|line| line.split('#').next().unwrap_or(line))
        .flat_map(|line| line.split("path").skip(1))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .filter_map(|rest| rest.trim_start().strip_prefix('"'))
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_string)
        .collect()
}

fn verify_target_stage(stage_dir: &Path, skip_simulator: bool) -> Result<()> {
    let mut required_paths = vec![
        stage_dir.join("bin").join("foundation"),
        stage_dir.join("bin").join("foundation-asset-tool"),
        stage_dir.join("bin").join("fatfs-image"),
        stage_dir.join("bin").join("foundation-slint-viewer"),
        stage_dir.join("bin").join("foundation-keyos-log-viewer"),
        stage_dir.join("bin").join("foundation-passport-drive"),
        stage_dir.join("bin").join("foundation-theme-editor"),
        stage_dir.join("bin").join("cosign2"),
        stage_dir.join("manifest.toml"),
    ];

    if !skip_simulator {
        required_paths.push(stage_dir.join("bin").join("foundation-simulator"));
        required_paths.push(
            stage_dir
                .join(KEYOS_HOSTED_RUNTIME_ROOT)
                .join("xous")
                .join("kernel")
                .join(KEYOS_HOSTED_KERNEL_PACKAGE),
        );
        required_paths
            .push(stage_dir.join(KEYOS_HOSTED_RUNTIME_ROOT).join("ui").join("ui").join("theme.slint"));
        required_paths.push(
            stage_dir.join(KEYOS_HOSTED_RUNTIME_ROOT).join("ui").join("ui").join("icons").join("loader.svg"),
        );
        required_paths.push(
            stage_dir
                .join(KEYOS_HOSTED_RUNTIME_ROOT)
                .join("ui")
                .join("ui")
                .join("images")
                .join("background.png"),
        );
        required_paths.push(
            stage_dir.join(KEYOS_HOSTED_RUNTIME_ROOT).join("resources").join("icons").join("loader.svg"),
        );
    }

    let missing = required_paths.into_iter().filter(|path| !path.exists()).collect::<Vec<_>>();

    if missing.is_empty() {
        // Services come from services.json; require at least one staged binary.
        if !skip_simulator {
            let bin_dir = stage_dir.join(KEYOS_HOSTED_RUNTIME_ROOT).join("bin");
            if fs::read_dir(&bin_dir).ok().and_then(|mut entries| entries.next()).is_none() {
                return Err(boxed_err(format!(
                    "staged simulator runtime at {} has no service binaries in {}",
                    stage_dir.display(),
                    bin_dir.display()
                )));
            }
        }
        return Ok(());
    }

    Err(boxed_err(format!(
        "staged target SDK content at {} is missing required paths:\n{}",
        stage_dir.display(),
        missing.iter().map(|path| format!("- {}", path.display())).collect::<Vec<_>>().join("\n")
    )))
}

fn ensure_layout(root: &Path, config: &Config, resolver: &SourceResolver, verbose: bool) -> Result<()> {
    let report = validate_layout(root, config, resolver);
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    if !report.errors.is_empty() {
        return Err(boxed_err(report.errors.join("\n")));
    }
    if verbose {
        eprintln!("validated sdk build layout");
    }
    Ok(())
}

fn validate_layout(root: &Path, config: &Config, resolver: &SourceResolver) -> LayoutReport {
    let mut report = LayoutReport::default();
    let copy_entries = config.expanded_copy_entries();
    let keyos_root = resolver.keyos_root();
    let manifest = keyos_root.join("Cargo.toml");
    if !manifest.exists() {
        report
            .errors
            .push(format!("repo-mode KeyOS source root is missing Cargo.toml at {}", manifest.display()));
    }
    validate_shared_ui_sources(keyos_root, &mut report);
    validate_slint_alignment(keyos_root, config, resolver, &mut report);

    for entry in &config.compile {
        let manifest_dir = resolver.resolve_source(root, &entry.manifest);
        let manifest_path = manifest_dir.join("Cargo.toml");
        if !manifest_path.exists() {
            let message = format!(
                "compile entry '{}' is missing Cargo.toml at {}",
                entry.name,
                manifest_path.display()
            );
            if entry.optional {
                report.warnings.push(message);
            } else {
                report.errors.push(message);
            }
        }
    }

    for entry in &copy_entries {
        let source = resolver.resolve_source(root, &entry.source);
        if !source.exists() {
            let message = format!("copy entry '{}' is missing source at {}", entry.dest, source.display());
            if entry.optional {
                report.warnings.push(message);
            } else {
                report.errors.push(message);
            }
        }
    }

    let guide_source = root.join(&config.docs.guide_source);
    if !guide_source.exists() {
        report.errors.push(format!("docs guide source is missing at {}", guide_source.display()));
    }

    report
}

fn validate_shared_ui_sources(keyos_root: &Path, report: &mut LayoutReport) {
    for relative in [SOURCE_UI_COMPONENTS_DIR, SOURCE_UI_RESOURCES_DIR] {
        let path = keyos_root.join(relative);
        if !path.is_dir() {
            report.errors.push(format!("shared SDK UI source directory is missing at {}", path.display()));
        }
    }
}

fn validate_slint_alignment(
    keyos_root: &Path,
    config: &Config,
    resolver: &SourceResolver,
    report: &mut LayoutReport,
) {
    let Some(slint_config) = config.submodules.get("slint") else {
        report.errors.push("sdk-build.toml is missing [submodules.slint]".to_string());
        return;
    };

    let pin = match load_keyos_slint_pin(keyos_root) {
        Ok(pin) => pin,
        Err(error) => {
            report.errors.push(error.to_string());
            return;
        }
    };

    if slint_config.r#ref != pin.tag {
        report.errors.push(format!(
            "SDK Slint ref '{}' does not match KeyOS Slint tag '{}'",
            slint_config.r#ref, pin.tag
        ));
    }

    let Some(resolved) = resolver.submodule("slint") else {
        report.errors.push("SDK Slint source was not resolved".to_string());
        return;
    };

    let resolved_path = resolved.path();
    if let Some(head) = git_revision(resolved_path) {
        if head != pin.commit {
            report.errors.push(format!(
                "resolved Slint source at {} is commit {}, expected {} from KeyOS Cargo.lock",
                resolved_path.display(),
                head,
                pin.commit
            ));
        }
    } else if !resolved_path.starts_with("/nix/store") {
        report.warnings.push(format!(
            "unable to verify exact Slint commit for {}; expected {} from KeyOS Cargo.lock",
            resolved_path.display(),
            pin.commit
        ));
    }
}

fn load_keyos_slint_pin(keyos_root: &Path) -> Result<KeyosSlintPin> {
    let manifest = fs::read_to_string(keyos_root.join("Cargo.toml"))?;
    let lockfile = fs::read_to_string(keyos_root.join("Cargo.lock"))?;
    keyos_slint_pin_from_manifest_and_lock(&manifest, &lockfile)
}

fn keyos_slint_pin_from_manifest_and_lock(manifest: &str, lockfile: &str) -> Result<KeyosSlintPin> {
    let manifest_value: toml::Value = toml::from_str(manifest)
        .map_err(|error| boxed_err(format!("failed to parse KeyOS Cargo.toml: {error}")))?;
    let tag = manifest_value
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("dependencies"))
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("slint"))
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("tag"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| boxed_err("failed to locate workspace.dependencies.slint.tag in KeyOS Cargo.toml"))?
        .to_string();

    let lock_value: toml::Value = toml::from_str(lockfile)
        .map_err(|error| boxed_err(format!("failed to parse KeyOS Cargo.lock: {error}")))?;
    let commit = lock_value
        .get("package")
        .and_then(toml::Value::as_array)
        .and_then(|packages| {
            packages.iter().find_map(|package| {
                let table = package.as_table()?;
                let name = table.get("name")?.as_str()?;
                let source = table.get("source")?.as_str()?;
                if name == "slint" && source.contains("Foundation-Devices/slint.git") {
                    parse_git_source_commit(source)
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| boxed_err("failed to locate locked Slint commit in KeyOS Cargo.lock"))?;

    Ok(KeyosSlintPin { tag, commit })
}

fn parse_git_source_commit(source: &str) -> Option<String> {
    source.rsplit_once('#').and_then(|(_, commit)| (!commit.is_empty()).then(|| commit.to_string()))
}

fn compile_entry(
    root: &Path,
    config: &Config,
    stage_dir: &Path,
    target: &str,
    host_target: &str,
    entry: &CompileEntry,
    args: &BuildArgs,
    resolver: &SourceResolver,
) -> Result<()> {
    let manifest_dir = resolver.resolve_source(root, &entry.manifest);
    let manifest_path = manifest_dir.join("Cargo.toml");
    if !manifest_path.exists() {
        if entry.optional {
            eprintln!(
                "warning: skipping optional compile entry '{}' because {} is missing",
                entry.name,
                manifest_path.display()
            );
            return Ok(());
        }

        if entry.manifest == "crates/cli" {
            return Err(boxed_err(
                "missing crates/cli/Cargo.toml; crates/cli is currently just a placeholder directory until the CLI code is added",
            ));
        }

        return Err(boxed_err(format!(
            "missing manifest for compile target '{}' at {}",
            entry.name,
            manifest_path.display()
        )));
    }

    let cargo_target_dir = root.join("target").join("xtask-build").join(target).join(&entry.name);
    let target_override = config.targets.overrides.get(target);
    let cargo_target = target_override
        .map(|target_override| target_override.cargo_target.as_str())
        .filter(|cargo_target| !cargo_target.is_empty())
        .unwrap_or(target);

    let mut command = cargo_command_for_manifest(&manifest_dir);
    command
        .current_dir(&manifest_dir)
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .arg("--target")
        .arg(cargo_target)
        .env("CARGO_TARGET_DIR", &cargo_target_dir);
    if let Some(package) = &entry.package {
        command.arg("-p").arg(package);
    }

    if args.release {
        command.arg("--release");
    }
    if let Some(jobs) = args.jobs {
        command.arg("--jobs").arg(jobs.to_string());
    }
    for flag in &entry.cargo_flags {
        command.arg(flag);
    }
    if let Some(target_override) = target_override {
        if !target_override.linker.is_empty() {
            command.env(cargo_target_linker_env(cargo_target), &target_override.linker);
        }
    }
    if should_disable_libusb_pkg_config_for_entry(&entry.name, target, host_target) {
        command.env("LIBUSB_1.0_NO_PKG_CONFIG", "1");
        command.env("LIBUDEV_NO_PKG_CONFIG", "1");
    }

    util::run_command(&mut command, args.verbose)?;

    let profile = if args.release { "release" } else { "debug" };
    let built_artifact = entry.artifact.as_deref().unwrap_or(&entry.binary);
    let built_binary = cargo_target_dir.join(cargo_target).join(profile).join(built_artifact);
    if !built_binary.exists() {
        return Err(boxed_err(format!(
            "expected compiled artifact '{}' for '{}' at {}",
            built_artifact,
            entry.name,
            built_binary.display()
        )));
    }

    let staged_binary = stage_dir.join("bin").join(&entry.binary);
    util::copy_file(&built_binary, &staged_binary)?;
    let strip_program = target_override
        .map(|target_override| target_override.strip.as_str())
        .filter(|strip| !strip.is_empty());
    maybe_strip_staged_binary(
        &staged_binary,
        should_strip_packaged_binaries(args),
        strip_program,
        target == host_target,
        args.verbose,
    )?;
    Ok(())
}

fn stage_simulator_runtime(
    root: &Path,
    _config: &Config,
    stage_dir: &Path,
    target: &str,
    entry: &CompileEntry,
    args: &BuildArgs,
    resolver: &SourceResolver,
) -> Result<()> {
    let keyos_root = resolver.keyos_root();
    let keyos_manifest = keyos_root.join("Cargo.toml");
    if !keyos_manifest.exists() {
        return Err(boxed_err(format!("missing KeyOS workspace manifest at {}", keyos_manifest.display())));
    }

    let cargo_target_dir = root.join("target").join("xtask-build").join(target).join(&entry.name);

    let keyos_xtask_manifest = keyos_root.join("xtask").join("Cargo.toml");
    let mut command = cargo_command_for_manifest(&keyos_root.join("xtask"));
    command
        .current_dir(keyos_root)
        .arg("run")
        .arg("--manifest-path")
        .arg(&keyos_xtask_manifest)
        .env("CARGO_TARGET_DIR", &cargo_target_dir);

    if let Some(jobs) = args.jobs {
        command.env("CARGO_BUILD_JOBS", jobs.to_string());
    }

    command.arg("--").arg("build").arg("--hosted").arg("--dont-sign");

    // Ship a pristine system image: drop any state or sideloaded apps a prior
    // local sim run left in the source image, since `build --hosted` recreates
    // and reseeds it only when absent. This clobbers the developer's local image.
    let built_system_image = keyos_root.join("xous").join("kernel").join("disk_system.dat");
    if built_system_image.exists() {
        fs::remove_file(&built_system_image)
            .map_err(|e| boxed_err(format!("remove {}: {e}", built_system_image.display())))?;
    }

    util::run_command(&mut command, args.verbose)?;

    let hosted_target_root = cargo_target_dir.join("hosted");
    let runtime_root = stage_dir.join(KEYOS_HOSTED_RUNTIME_ROOT);
    let runtime_bin_dir = runtime_root.join("bin");
    let runtime_kernel_dir = runtime_root.join("xous").join("kernel");
    // app-manager execs built-in binaries from <runtime_root>/apps (mirrors the
    // image's /keyos/apps); foundation sim points the app-elf root at runtime_root.
    let runtime_apps_dir = runtime_root.join("apps");
    let runtime_ui_dir = runtime_root.join("ui").join("ui");
    let runtime_ui_resources_dir = runtime_root.join("resources");
    let legacy_ui_dir = keyos_root.join(KEYOS_LEGACY_UI_SOURCE_ROOT);

    util::ensure_clean_dir(&runtime_bin_dir)?;
    util::ensure_clean_dir(&runtime_kernel_dir)?;
    util::ensure_dir(&runtime_apps_dir)?;
    util::ensure_clean_dir(&runtime_ui_dir)?;
    util::ensure_clean_dir(&runtime_ui_resources_dir)?;

    let built_kernel = hosted_target_root.join(KEYOS_HOSTED_KERNEL_PACKAGE);
    let staged_kernel = runtime_kernel_dir.join(KEYOS_HOSTED_KERNEL_PACKAGE);
    util::copy_file(&built_kernel, &staged_kernel)?;
    maybe_strip_staged_binary(
        &staged_kernel,
        should_strip_packaged_binaries(args),
        None,
        false,
        args.verbose,
    )?;

    // The hosted build populated the system image (UI assets + built-in apps)
    // next to the source kernel; ship it so a fresh bundle boots with assets.
    // The user volume (disk.dat) is created by the simulator launcher on first run.
    util::copy_file(&built_system_image, &runtime_kernel_dir.join("disk_system.dat"))?;

    // `xtask build --hosted` wrote the canonical services.json (path + app_id +
    // syscall mask) listing exactly the hosted service binaries it built; it is
    // the source of truth, so we don't duplicate the keyos-side service list.
    // Stage each binary into bin/ and rewrite its path to that bundle location
    // relative to the kernel's run dir, then ship the manifest as-is. The kernel
    // reads it as argv[1] and resolves the relative paths against its cwd, so the
    // manifest works wherever the bundle is unpacked.
    let build_manifest = hosted_target_root.join("services.json");
    let mut services: Vec<serde_json::Value> = serde_json::from_reader(
        fs::File::open(&build_manifest)
            .map_err(|e| boxed_err(format!("open {}: {e}", build_manifest.display())))?,
    )
    .map_err(|e| boxed_err(format!("parse {}: {e}", build_manifest.display())))?;
    for service in &mut services {
        let name = service["path"]
            .as_str()
            .and_then(|path| Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .ok_or_else(|| boxed_err(format!("bad service entry in {}", build_manifest.display())))?
            .to_owned();
        let staged_binary = runtime_bin_dir.join(&name);
        util::copy_file(&hosted_target_root.join(&name), &staged_binary)?;
        maybe_strip_staged_binary(
            &staged_binary,
            should_strip_packaged_binaries(args),
            None,
            false,
            args.verbose,
        )?;
        service["path"] = serde_json::Value::String(format!("../../bin/{name}"));
    }
    fs::write(runtime_root.join("services.json"), serde_json::to_string_pretty(&services)?)?;

    // build --hosted stages built-in bundles (manifest + app.elf) under the
    // hosted target root; ship the app.elf files the simulator execs.
    let built_apps_dir = hosted_target_root.join("keyos").join("apps");
    if built_apps_dir.exists() {
        util::copy_dir_contents(&built_apps_dir, &runtime_apps_dir)?;
        strip_staged_binaries_in_dir(&runtime_apps_dir, should_strip_packaged_binaries(args), args.verbose)?;
    }

    stage_shared_ui_components(keyos_root, &runtime_ui_dir)?;
    stage_shared_ui_resources(keyos_root, &runtime_ui_resources_dir)?;
    stage_legacy_simulator_ui_assets(&legacy_ui_dir, &runtime_ui_dir)?;

    write_simulator_launcher(&stage_dir.join("bin").join(&entry.binary), KEYOS_HOSTED_RUNTIME_ROOT)?;

    Ok(())
}

fn stage_legacy_simulator_ui_assets(source_root: &Path, destination_root: &Path) -> Result<()> {
    for asset_dir in KEYOS_LEGACY_UI_ASSET_DIRS {
        let source = source_root.join(asset_dir);
        if source.exists() {
            util::copy_dir_all(&source, &destination_root.join(asset_dir))?;
        }
    }

    Ok(())
}

fn should_strip_packaged_binaries(args: &BuildArgs) -> bool { args.package || args.release }

fn maybe_strip_staged_binary(
    path: &Path,
    should_strip: bool,
    configured_strip_program: Option<&str>,
    allow_default_fallback: bool,
    verbose: bool,
) -> Result<()> {
    if !should_strip || !path.is_file() || !is_strippable_binary(path)? {
        return Ok(());
    }

    let strip_program = find_strip_program(configured_strip_program, allow_default_fallback).ok_or_else(|| {
        match (configured_strip_program, allow_default_fallback) {
            (Some(program), true) => boxed_err(format!(
                "packaged native SDK binaries require configured strip program '{program}' or fallback 'strip'/'llvm-strip'"
            )),
            (Some(program), false) => {
                boxed_err(format!("packaged SDK binaries require configured strip program '{program}'"))
            }
            (None, _) => boxed_err(
                "packaged SDK binaries require 'strip' or 'llvm-strip' to remove bundled debug info",
            ),
        }
    })?;
    let mut command = Command::new(strip_program);
    if cfg!(target_os = "macos") {
        command.arg("-S");
    } else {
        command.arg("--strip-debug");
    }
    command.arg(path);
    util::run_command(&mut command, verbose)
}

fn strip_staged_binaries_in_dir(dir: &Path, should_strip: bool, verbose: bool) -> Result<()> {
    if !should_strip || !dir.exists() {
        return Ok(());
    }

    let mut entries = fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            strip_staged_binaries_in_dir(&path, should_strip, verbose)?;
        } else {
            maybe_strip_staged_binary(&path, should_strip, None, false, verbose)?;
        }
    }

    Ok(())
}

fn find_strip_program(configured: Option<&str>, allow_default_fallback: bool) -> Option<PathBuf> {
    strip_program_candidates(configured, allow_default_fallback).into_iter().find_map(find_program_in_path)
}

fn strip_program_candidates(configured: Option<&str>, allow_default_fallback: bool) -> Vec<&str> {
    let mut candidates = configured.into_iter().collect::<Vec<_>>();
    if configured.is_none() || allow_default_fallback {
        candidates.extend(["strip", "llvm-strip"]);
    }
    candidates
}

fn find_program_in_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths).find_map(|dir| {
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}

fn is_strippable_binary(path: &Path) -> Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 4];
    let bytes_read = file.read(&mut header)?;
    if bytes_read < header.len() {
        return Ok(false);
    }

    Ok(is_strippable_binary_header(header))
}

fn is_strippable_binary_header(header: [u8; 4]) -> bool {
    matches!(
        header,
        [0x7f, b'E', b'L', b'F']
            | [0xfe, 0xed, 0xfa, 0xce]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    )
}

fn write_simulator_launcher(path: &Path, runtime_root_rel: &str) -> Result<()> {
    let mut script = String::new();
    script.push_str("#!/usr/bin/env bash\n");
    script.push_str("set -euo pipefail\n");
    script.push_str("SCRIPT_DIR=\"$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\"\n");
    script.push_str("SDK_ROOT=\"$(CDPATH= cd -- \"$SCRIPT_DIR/..\" && pwd)\"\n");
    script.push_str(&format!("RUNTIME_ROOT=\"$SDK_ROOT/{runtime_root_rel}\"\n"));
    script.push_str("KERNEL_DIR=\"$RUNTIME_ROOT/xous/kernel\"\n");
    script.push_str(&format!("KERNEL_BIN=\"$KERNEL_DIR/{}\"\n", KEYOS_HOSTED_KERNEL_PACKAGE));
    script.push_str("if [ ! -x \"$KERNEL_BIN\" ]; then\n");
    script.push_str("  echo \"foundation-simulator: missing hosted kernel at $KERNEL_BIN\" >&2\n");
    script.push_str("  exit 1\n");
    script.push_str("fi\n");
    // os/fs opens disk.dat unconditionally; the bundle ships only the system
    // image, so create the user volume here or a direct launch (not driven by
    // `foundation sim`) panics in fs before boot. fatfs-image ships alongside.
    script.push_str("USER_IMAGE=\"$KERNEL_DIR/disk.dat\"\n");
    script.push_str("if [ ! -f \"$USER_IMAGE\" ]; then\n");
    script.push_str("  \"$SCRIPT_DIR/fatfs-image\" create \"$USER_IMAGE\" --size 8G --label USER\n");
    script.push_str("fi\n");
    // app-manager execs host app binaries from the runtime root (mirrors /keyos);
    // export it so the bundle launches built-ins even without foundation sim.
    script.push_str("export FOUNDATION_SIMULATOR_APP_ELF_ROOT=\"$RUNTIME_ROOT\"\n");
    // The kernel takes services.json as argv[1]; its service paths are relative to
    // this run dir, so the shipped manifest works wherever the bundle is unpacked.
    script.push_str("cd \"$KERNEL_DIR\"\n");
    script.push_str("exec \"$KERNEL_BIN\" \"$RUNTIME_ROOT/services.json\"\n");

    fs::write(path, script)?;
    set_executable(path)?;
    Ok(())
}

fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }

    Ok(())
}

fn cargo_command_for_manifest(manifest_dir: &Path) -> Command {
    if nix_shell_active() {
        let mut command = Command::new("cargo");
        apply_host_macos_developer_dir(&mut command);
        command.env_remove("RUSTUP_TOOLCHAIN");
        command.env_remove("RUSTUP_OVERRIDE_TOOLCHAIN");
        return command;
    }

    if let Some(toolchain) = rust_toolchain_channel(manifest_dir) {
        if util::command_exists("rustup") {
            if let Some(cargo_path) = rustup_tool_path(&toolchain, "cargo") {
                let mut command = Command::new(cargo_path);
                if let Some(rustc_path) = rustup_tool_path(&toolchain, "rustc") {
                    command.env("RUSTC", rustc_path);
                }
                if let Some(rustdoc_path) = rustup_tool_path(&toolchain, "rustdoc") {
                    command.env("RUSTDOC", rustdoc_path);
                }
                apply_host_macos_developer_dir(&mut command);
                command.env_remove("RUSTUP_TOOLCHAIN");
                command.env_remove("RUSTUP_OVERRIDE_TOOLCHAIN");
                return command;
            }

            let mut command = Command::new("rustup");
            command.arg("run").arg(toolchain).arg("cargo");
            apply_host_macos_developer_dir(&mut command);
            command.env_remove("RUSTUP_TOOLCHAIN");
            command.env_remove("RUSTUP_OVERRIDE_TOOLCHAIN");
            return command;
        }
    }

    let mut command = Command::new("cargo");
    apply_host_macos_developer_dir(&mut command);
    command.env_remove("RUSTUP_TOOLCHAIN");
    command.env_remove("RUSTUP_OVERRIDE_TOOLCHAIN");
    command
}

fn nix_shell_active() -> bool { env::var_os("FOUNDATION_DEVELOP_SHELL").is_some() }

fn rustup_tool_path(toolchain: &str, tool: &str) -> Option<PathBuf> {
    util::capture_command(Command::new("rustup").arg("which").arg("--toolchain").arg(toolchain).arg(tool))
        .ok()
        .map(PathBuf::from)
}

fn apply_host_macos_developer_dir(command: &mut Command) {
    if cfg!(target_os = "macos") {
        command.env_remove("DEVELOPER_DIR");
        command.env_remove("SDKROOT");

        if let Ok(path) = util::capture_command(Command::new("/usr/bin/xcode-select").arg("-p")) {
            if !path.is_empty() {
                command.env("DEVELOPER_DIR", path);
            }
        }

        if let Ok(path) =
            util::capture_command(Command::new("/usr/bin/xcrun").args(["--sdk", "macosx", "--show-sdk-path"]))
        {
            if !path.is_empty() {
                command.env("SDKROOT", path);
            }
        }
    }
}

fn rust_toolchain_channel(start: &Path) -> Option<String> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let toolchain_file = dir.join("rust-toolchain.toml");
        if toolchain_file.exists() {
            return parse_toolchain_channel(&toolchain_file);
        }
        current = dir.parent();
    }
    None
}

fn parse_toolchain_channel(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("channel") {
            let value = value.trim_start();
            let value = value.strip_prefix('=')?.trim();
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

fn cargo_target_linker_env(target: &str) -> String {
    format!(
        "CARGO_TARGET_{}_LINKER",
        target
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    )
}

fn should_disable_libusb_pkg_config_for_entry(entry_name: &str, target: &str, host_target: &str) -> bool {
    entry_name == "keyos-log-viewer" && target.ends_with("-unknown-linux-gnu") && target != host_target
}

fn copy_entry(root: &Path, stage_dir: &Path, entry: &CopyEntry, resolver: &SourceResolver) -> Result<()> {
    let source = resolver.resolve_source(root, &entry.source);
    if !source.exists() {
        if entry.optional {
            eprintln!(
                "warning: skipping optional copy entry '{}' because {} is missing",
                entry.dest,
                source.display()
            );
            return Ok(());
        }

        return Err(boxed_err(format!("missing copy source '{}' at {}", entry.source, source.display())));
    }

    let destination = stage_dir.join(&entry.dest);
    if source.is_file() {
        return util::copy_file(&source, &destination);
    }
    match entry.filter {
        CopyFilter::All => util::copy_dir_all(&source, &destination)?,
        CopyFilter::CargoPackage => stage_cargo_package_snapshot(&source, &destination)?,
        CopyFilter::SlintSdk => stage_slint_sdk_snapshot(&source, &destination)?,
    }
    Ok(())
}

fn stage_bt_error_placeholder(keyos_root: &Path, stage_dir: &Path) -> Result<()> {
    let source_error = keyos_root.join("api/bt/src/error.rs");
    if !source_error.is_file() {
        return Err(boxed_err(format!(
            "quantum-link requires the BluetoothError source at {}",
            source_error.display()
        )));
    }

    let destination = stage_dir.join(BT_PLACEHOLDER_DESTINATION);
    util::ensure_clean_dir(&destination)?;
    util::ensure_dir(&destination.join("src"))?;
    fs::write(destination.join("Cargo.toml"), BT_PLACEHOLDER_MANIFEST)?;
    fs::write(destination.join("src/lib.rs"), render_bt_placeholder_lib(keyos_root)?)?;
    util::copy_file(&source_error, &destination.join("src/error.rs"))?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct ScalarEnumVariant {
    name: String,
    discriminant: u32,
}

/// Render only the GPIO and SPI error payloads that BluetoothError needs for
/// its serialized KeyOS-only variants. The definitions come from the selected
/// source checkout, which keeps an overridden KEYOS_DIR wire-compatible.
fn render_bt_placeholder_lib(keyos_root: &Path) -> Result<String> {
    let gpio_source = read_bt_payload_source(keyos_root, "api/gpio/src/lib.rs")?;
    let spi_source = read_bt_payload_source(keyos_root, "api/spi/src/error.rs")?;
    let gpio = parse_scalar_error_enum(&gpio_source, "GpioApiError")?;
    let spi = parse_scalar_error_enum(&spi_source, "SpiError")?;

    Ok(format!(
        "{BT_PLACEHOLDER_LIB}\n{}\n{}",
        render_scalar_error_enum("GpioApiError", &gpio),
        render_scalar_error_enum("SpiError", &spi),
    ))
}

fn read_bt_payload_source(keyos_root: &Path, relative_path: &str) -> Result<String> {
    let path = keyos_root.join(relative_path);
    fs::read_to_string(&path).map_err(|error| {
        boxed_err(format!("could not read BluetoothError payload source {}: {error}", path.display()))
    })
}

fn parse_scalar_error_enum(source: &str, enum_name: &str) -> Result<Vec<ScalarEnumVariant>> {
    let declaration = format!("pub enum {enum_name}");
    let declaration_start = source
        .find(&declaration)
        .ok_or_else(|| boxed_err(format!("could not find {declaration} in selected KeyOS source")))?;
    let opening_brace = source[declaration_start..]
        .find('{')
        .map(|offset| declaration_start + offset)
        .ok_or_else(|| boxed_err(format!("{declaration} has no opening brace")))?;
    let closing_brace = matching_closing_brace(source, opening_brace)
        .ok_or_else(|| boxed_err(format!("{declaration} has no closing brace")))?;
    let body = strip_rust_comments(&source[opening_brace + 1..closing_brace]);
    let mut next_discriminant = 0_u32;
    let mut variants = Vec::new();

    for entry in body.split(',') {
        let entry = entry
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("#["))
            .collect::<Vec<_>>()
            .join(" ");
        if entry.is_empty() {
            continue;
        }

        let (name, discriminant) = match entry.split_once('=') {
            Some((name, value)) => (name.trim(), parse_enum_discriminant(value, enum_name)?),
            None => (entry.trim(), next_discriminant),
        };
        if !is_rust_identifier(name) {
            return Err(boxed_err(format!(
                "{enum_name} has unsupported non-unit variant '{entry}' in selected KeyOS source"
            )));
        }
        if variants.iter().any(|variant: &ScalarEnumVariant| variant.name == name) {
            return Err(boxed_err(format!("{enum_name} repeats variant '{name}' in selected KeyOS source")));
        }
        variants.push(ScalarEnumVariant { name: name.to_owned(), discriminant });
        next_discriminant = discriminant.checked_add(1).ok_or_else(|| {
            boxed_err(format!("{enum_name} variant '{name}' overflows its u32 discriminant"))
        })?;
    }

    if variants.is_empty() {
        return Err(boxed_err(format!("{enum_name} has no variants in selected KeyOS source")));
    }
    if !variants.iter().any(|variant| variant.name == "InternalError") {
        return Err(boxed_err(format!("{enum_name} has no InternalError fallback in selected KeyOS source")));
    }
    Ok(variants)
}

fn matching_closing_brace(source: &str, opening_brace: usize) -> Option<usize> {
    let mut depth = 0_usize;
    for (offset, byte) in source.as_bytes()[opening_brace..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(opening_brace + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_rust_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut stripped = String::new();
    let mut copied_from = 0;
    let mut index = 0;

    while index + 1 < bytes.len() {
        match (bytes[index], bytes[index + 1]) {
            (b'/', b'/') => {
                stripped.push_str(&source[copied_from..index]);
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                copied_from = index;
            }
            (b'/', b'*') => {
                stripped.push_str(&source[copied_from..index]);
                index += 2;
                let mut depth = 1;
                while index < bytes.len() && depth > 0 {
                    if index + 1 < bytes.len() && bytes[index..].starts_with(b"/*") {
                        depth += 1;
                        index += 2;
                    } else if index + 1 < bytes.len() && bytes[index..].starts_with(b"*/") {
                        depth -= 1;
                        index += 2;
                    } else {
                        if bytes[index] == b'\n' {
                            stripped.push('\n');
                        }
                        index += 1;
                    }
                }
                copied_from = index;
            }
            _ => index += 1,
        }
    }
    stripped.push_str(&source[copied_from..]);
    stripped
}

fn parse_enum_discriminant(value: &str, enum_name: &str) -> Result<u32> {
    let value = value.trim();
    if value.contains(char::is_whitespace) {
        return Err(boxed_err(format!(
            "{enum_name} has unsupported discriminant expression '{value}' in selected KeyOS source"
        )));
    }
    let value = value.strip_suffix("u32").unwrap_or(value).replace('_', "");
    value.parse::<u32>().map_err(|error| {
        boxed_err(format!("{enum_name} has invalid discriminant '{value}' in selected KeyOS source: {error}"))
    })
}

fn is_rust_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some(character) if character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn render_scalar_error_enum(enum_name: &str, variants: &[ScalarEnumVariant]) -> String {
    let declarations = variants
        .iter()
        .map(|variant| format!("    {} = {},", variant.name, variant.discriminant))
        .collect::<Vec<_>>()
        .join("\n");
    let matches = variants
        .iter()
        .map(|variant| format!("            {} => Self::{},", variant.discriminant, variant.name))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"#[cfg(keyos)]
#[derive(Debug, Copy, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum {enum_name} {{
{declarations}
}}

#[cfg(keyos)]
impl server::AsScalar<1> for {enum_name} {{
    fn as_scalar(&self) -> [u32; 1] {{ [*self as u32] }}
}}

#[cfg(keyos)]
impl server::FromScalar<1> for {enum_name} {{
    fn from_scalar([value]: [u32; 1]) -> Self {{
        match value {{
{matches}
            _ => Self::InternalError,
        }}
    }}
}}
"#
    )
}

fn stage_shared_ui_artifact(keyos_root: &Path, stage_dir: &Path, verbose: bool) -> Result<()> {
    let copied_ui_files = stage_shared_ui_components(keyos_root, &stage_dir.join(STAGED_SDK_UI_ROOT))?;
    stage_shared_ui_components(keyos_root, &stage_dir.join(STAGED_KEYOS_SDK_UI_ROOT))?;
    stage_shared_ui_resources(keyos_root, &stage_dir.join(STAGED_SDK_RESOURCES_ROOT))?;
    // The SDK theme JSON ships as part of the foundation-themes crate itself
    // (the [[copy]] cargo_package snapshot in sdk-build.toml includes its
    // `themes/` dir). foundation-themes/themes is the single source of truth,
    // so there's no separate theme-staging step.

    if verbose {
        eprintln!(
            "staged {copied_ui_files} shared SDK UI files from {}",
            keyos_root.join(SOURCE_UI_COMPONENTS_DIR).display()
        );
        eprintln!("staged shared SDK resources from {}", keyos_root.join(SOURCE_UI_RESOURCES_DIR).display());
    }

    Ok(())
}

fn stage_shared_ui_components(keyos_root: &Path, destination_ui_dir: &Path) -> Result<usize> {
    let source_ui_dir = keyos_root.join(SOURCE_UI_COMPONENTS_DIR);
    if !source_ui_dir.is_dir() {
        return Err(boxed_err(format!(
            "shared UI component directory not found at {}",
            source_ui_dir.display()
        )));
    }

    let source_workspace_name = keyos_root
        .join("ui2")
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| boxed_err("shared UI source workspace path is missing a final directory name"))?
        .to_string();
    let legacy_resource_prefix = format!("../../resources/{source_workspace_name}/");

    util::ensure_clean_dir(destination_ui_dir)?;

    let mut copied_ui_files = 0usize;
    let mut entries = fs::read_dir(&source_ui_dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let source_path = entry.path();
        if !source_path.is_file() || source_path.extension().and_then(|ext| ext.to_str()) != Some("slint") {
            continue;
        }

        let contents = fs::read_to_string(&source_path)?;
        let rewritten = contents.replace(&legacy_resource_prefix, "../../resources/");
        fs::write(destination_ui_dir.join(entry.file_name()), rewritten)?;
        copied_ui_files += 1;
    }

    Ok(copied_ui_files)
}

fn stage_shared_ui_resources(keyos_root: &Path, destination_resources_dir: &Path) -> Result<()> {
    let source_resources_dir = keyos_root.join(SOURCE_UI_RESOURCES_DIR);
    if !source_resources_dir.is_dir() {
        return Err(boxed_err(format!(
            "shared UI resources directory not found at {}",
            source_resources_dir.display()
        )));
    }

    util::ensure_clean_dir(destination_resources_dir)?;
    util::copy_dir_contents(&source_resources_dir, destination_resources_dir)
}

fn stage_keyos_workspace_root(
    root: &Path,
    _config: &Config,
    stage_dir: &Path,
    copy_entries: &[CopyEntry],
    resolver: &SourceResolver,
) -> Result<()> {
    let staged_keyos_root = stage_dir.join("lib").join("keyos");
    if !staged_keyos_root.is_dir() {
        return Ok(());
    }

    let keyos_source_root = resolver.keyos_root();
    let member_dirs = collect_staged_keyos_member_dirs(root, resolver, copy_entries)?;
    let manifest =
        render_staged_keyos_workspace_manifest(keyos_source_root, &staged_keyos_root, &member_dirs)?;
    fs::write(staged_keyos_root.join("Cargo.toml"), manifest)?;
    Ok(())
}

fn collect_staged_keyos_member_dirs(
    root: &Path,
    resolver: &SourceResolver,
    copy_entries: &[CopyEntry],
) -> Result<Vec<String>> {
    let mut members = BTreeSet::new();

    for entry in copy_entries {
        if !should_scan_for_keyos_member(&entry.dest) {
            continue;
        }

        let source_root = resolver.resolve_source(root, &entry.source);
        if !source_root.exists() {
            if entry.optional {
                continue;
            }
            return Err(boxed_err(format!(
                "missing copy source '{}' at {}",
                entry.source,
                source_root.display()
            )));
        }

        let staged_root = PathBuf::from(entry.dest.strip_prefix("lib/keyos/").unwrap_or(&entry.dest));
        if !source_root.is_dir() {
            continue;
        }
        scan_member_dirs(&source_root, &source_root, &staged_root, &mut members)?;
    }

    Ok(members.into_iter().collect())
}

fn should_scan_for_keyos_member(dest: &str) -> bool {
    is_path_or_child(dest, "lib/keyos")
        && !is_path_or_child(dest, "lib/keyos/keyos")
        && !is_path_or_child(dest, STAGED_KEYOS_SDK_UI_ROOT)
        && !is_path_or_child(dest, KEYOS_HOSTED_RUNTIME_ROOT)
}

fn is_path_or_child(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).map(|suffix| suffix.starts_with('/')).unwrap_or(false)
}

fn scan_member_dirs(
    current: &Path,
    base: &Path,
    staged_root: &Path,
    members: &mut BTreeSet<String>,
) -> Result<()> {
    let cargo_toml = current.join("Cargo.toml");
    if cargo_toml.exists() {
        let relative = current.strip_prefix(base).map_err(|error| {
            boxed_err(format!(
                "failed to relativize {} against {}: {error}",
                current.display(),
                base.display()
            ))
        })?;
        let staged_member = if relative.as_os_str().is_empty() {
            staged_root.to_path_buf()
        } else {
            staged_root.join(relative)
        };
        let normalized = staged_member.to_string_lossy().replace('\\', "/");
        if !normalized.is_empty() {
            members.insert(normalized);
        }
    }

    let mut entries = fs::read_dir(current)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            scan_member_dirs(&path, base, staged_root, members)?;
        }
    }

    Ok(())
}

fn stage_slint_sdk_snapshot(source_root: &Path, destination_root: &Path) -> Result<()> {
    util::ensure_clean_dir(destination_root)?;

    for relative_file in SLINT_SDK_ROOT_FILES {
        util::copy_file(&source_root.join(relative_file), &destination_root.join(relative_file)).map_err(
            |error| {
                boxed_err(format!(
                    "failed to copy Slint SDK root file '{}' from {} to {}: {error}",
                    relative_file,
                    source_root.display(),
                    destination_root.display()
                ))
            },
        )?;
    }

    for relative_dir in SLINT_SDK_ROOT_DIRS {
        copy_dir_all_excluding_names(
            &source_root.join(relative_dir),
            &destination_root.join(relative_dir),
            SLINT_SDK_EXCLUDED_DIR_NAMES,
        )
        .map_err(|error| {
            boxed_err(format!(
                "failed to copy Slint SDK root directory '{}' from {} to {}: {error}",
                relative_dir,
                source_root.display(),
                destination_root.display()
            ))
        })?;
    }

    let member_dirs = collect_slint_sdk_member_dirs(source_root)?;
    for member in &member_dirs {
        copy_dir_all_excluding_names(
            &source_root.join(member),
            &destination_root.join(member),
            SLINT_SDK_EXCLUDED_DIR_NAMES,
        )
        .map_err(|error| {
            boxed_err(format!(
                "failed to copy Slint SDK member '{}' from {} to {}: {error}",
                member,
                source_root.display(),
                destination_root.display()
            ))
        })?;
    }

    let manifest = render_staged_slint_workspace_manifest(source_root, destination_root, &member_dirs)
        .map_err(|error| {
            boxed_err(format!(
                "failed to render staged Slint workspace manifest from {} into {}: {error}",
                source_root.display(),
                destination_root.display()
            ))
        })?;
    fs::write(destination_root.join("Cargo.toml"), manifest).map_err(|error| {
        boxed_err(format!(
            "failed to write staged Slint workspace manifest at {}: {error}",
            destination_root.join("Cargo.toml").display()
        ))
    })?;
    Ok(())
}

fn stage_cargo_package_snapshot(source_root: &Path, destination_root: &Path) -> Result<()> {
    util::ensure_clean_dir(destination_root)?;
    copy_dir_all_excluding_names(source_root, destination_root, CARGO_PACKAGE_EXCLUDED_DIR_NAMES)
}

fn collect_slint_sdk_member_dirs(source_root: &Path) -> Result<Vec<String>> {
    let mut members = BTreeSet::new();
    let mut pending = SLINT_SDK_SEED_DIRS.iter().map(|value| value.to_string()).collect::<Vec<_>>();

    while let Some(member) = pending.pop() {
        if !members.insert(member.clone()) {
            continue;
        }

        let manifest_path = source_root.join(&member).join("Cargo.toml");
        if !manifest_path.exists() {
            return Err(boxed_err(format!(
                "Slint SDK snapshot member '{}' is missing Cargo.toml at {}",
                member,
                manifest_path.display()
            )));
        }

        for dependency_dir in
            collect_local_path_dependency_dirs(&manifest_path, source_root).map_err(|error| {
                boxed_err(format!(
                    "failed to resolve local path dependencies for Slint SDK member '{}' at {}: {error}",
                    member,
                    manifest_path.display()
                ))
            })?
        {
            let normalized = dependency_dir.to_string_lossy().replace('\\', "/");
            if !members.contains(&normalized) {
                pending.push(normalized);
            }
        }
    }

    Ok(prune_nested_member_dirs(members.into_iter().collect()))
}

fn prune_nested_member_dirs(mut members: Vec<String>) -> Vec<String> {
    members.sort();
    let snapshot = members.clone();
    members.retain(|candidate| {
        !snapshot.iter().any(|other| other != candidate && candidate.starts_with(&format!("{other}/")))
    });
    members
}

fn collect_local_path_dependency_dirs(manifest_path: &Path, workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let contents = fs::read_to_string(manifest_path)?;
    let manifest: toml::Value = toml::from_str(&contents)
        .map_err(|error| boxed_err(format!("failed to parse {}: {error}", manifest_path.display())))?;
    let workspace_manifest_path = workspace_root.join("Cargo.toml");
    let workspace_manifest_contents = fs::read_to_string(&workspace_manifest_path)?;
    let workspace_manifest: toml::Value = toml::from_str(&workspace_manifest_contents).map_err(|error| {
        boxed_err(format!(
            "failed to parse Slint workspace manifest {}: {error}",
            workspace_manifest_path.display()
        ))
    })?;
    let empty_workspace_dependencies = toml::map::Map::new();
    let workspace_dependencies = workspace_manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table)
        .unwrap_or(&empty_workspace_dependencies);
    let manifest_dir = manifest_path
        .parent()
        .ok_or_else(|| boxed_err(format!("manifest path has no parent: {}", manifest_path.display())))?;
    let normalized_workspace_root = fs::canonicalize(workspace_root).map_err(|error| {
        boxed_err(format!(
            "failed to canonicalize Slint workspace root {}: {error}",
            workspace_root.display()
        ))
    })?;
    let mut paths = BTreeSet::new();
    collect_local_path_dependency_dirs_from_value(
        &manifest,
        manifest_dir,
        &normalized_workspace_root,
        workspace_dependencies,
        &mut paths,
    )?;
    Ok(paths.into_iter().collect())
}

fn collect_local_path_dependency_dirs_from_value(
    value: &toml::Value,
    manifest_dir: &Path,
    workspace_root: &Path,
    workspace_dependencies: &toml::map::Map<String, toml::Value>,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                if let Some(dependency_table) = child.as_table() {
                    if let Some(path_value) = dependency_table.get("path").and_then(toml::Value::as_str) {
                        insert_local_dependency_dir(path_value, manifest_dir, workspace_root, paths)
                            .map_err(|error| {
                                boxed_err(format!(
                                    "failed to resolve local path dependency '{}' in {}: {error}",
                                    path_value,
                                    manifest_dir.display()
                                ))
                            })?;
                    }

                    if dependency_table.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                        if let Some(path_value) = workspace_dependencies
                            .get(key)
                            .and_then(toml::Value::as_table)
                            .and_then(|workspace_dep| workspace_dep.get("path"))
                            .and_then(toml::Value::as_str)
                        {
                            insert_local_dependency_dir(path_value, workspace_root, workspace_root, paths)
                                .map_err(|error| {
                                    boxed_err(format!(
                                        "failed to resolve workspace dependency '{}' from {}: {error}",
                                        key,
                                        manifest_dir.display()
                                    ))
                                })?;
                        }
                    }
                }

                collect_local_path_dependency_dirs_from_value(
                    child,
                    manifest_dir,
                    workspace_root,
                    workspace_dependencies,
                    paths,
                )?;
            }
        }
        toml::Value::Array(values) => {
            for child in values {
                collect_local_path_dependency_dirs_from_value(
                    child,
                    manifest_dir,
                    workspace_root,
                    workspace_dependencies,
                    paths,
                )?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn insert_local_dependency_dir(
    path_value: &str,
    base_dir: &Path,
    workspace_root: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let candidate = match fs::canonicalize(base_dir.join(path_value)) {
        Ok(candidate) => candidate,
        Err(_) => return Ok(()),
    };
    if candidate.join("Cargo.toml").exists() {
        let relative = candidate.strip_prefix(workspace_root).map_err(|error| {
            boxed_err(format!(
                "local path dependency '{}' resolves outside {}: {error}",
                path_value,
                workspace_root.display()
            ))
        })?;
        paths.insert(relative.to_path_buf());
    }

    Ok(())
}

fn copy_dir_all_excluding_names(
    source: &Path,
    destination: &Path,
    excluded_dir_names: &[&str],
) -> Result<()> {
    if !source.exists() {
        return Err(boxed_err(format!("copy source does not exist: {}", source.display())));
    }

    fs::create_dir_all(destination)?;

    let mut entries = fs::read_dir(source)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let destination_path = destination.join(entry.file_name());
        if path.is_dir() {
            if excluded_dir_names.iter().any(|excluded| *excluded == file_name) {
                continue;
            }
            copy_dir_all_excluding_names(&path, &destination_path, excluded_dir_names)?;
        } else {
            util::copy_file(&path, &destination_path)?;
        }
    }

    Ok(())
}

fn render_staged_slint_workspace_manifest(
    source_root: &Path,
    staged_root: &Path,
    member_dirs: &[String],
) -> Result<String> {
    let source_manifest_path = source_root.join("Cargo.toml");
    let source_manifest_contents = fs::read_to_string(&source_manifest_path)?;
    let mut source_manifest: toml::Value = toml::from_str(&source_manifest_contents)
        .map_err(|error| boxed_err(format!("failed to parse {}: {error}", source_manifest_path.display())))?;

    let workspace = source_manifest
        .get_mut("workspace")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| boxed_err(format!("{} is missing [workspace]", source_manifest_path.display())))?;

    workspace.insert(
        "members".to_string(),
        toml::Value::Array(member_dirs.iter().map(|member| toml::Value::String(member.clone())).collect()),
    );

    if let Some(default_members) = workspace.get_mut("default-members").and_then(toml::Value::as_array_mut) {
        default_members.retain(|value| {
            value
                .as_str()
                .map(|member| member_dirs.iter().any(|candidate| candidate == member))
                .unwrap_or(false)
        });
    }

    if let Some(dependencies) = workspace.get_mut("dependencies").and_then(toml::Value::as_table_mut) {
        dependencies.retain(|_, spec| {
            spec.as_table()
                .and_then(|table| table.get("path"))
                .and_then(toml::Value::as_str)
                .map(|relative_path| staged_root.join(relative_path).exists())
                .unwrap_or(true)
        });
    }

    toml::to_string(&source_manifest)
        .map_err(|error| boxed_err(format!("failed to render staged Slint workspace: {error}")))
}

fn render_staged_keyos_workspace_manifest(
    keyos_source_root: &Path,
    staged_keyos_root: &Path,
    member_dirs: &[String],
) -> Result<String> {
    let source_manifest_path = keyos_source_root.join("Cargo.toml");
    let source_manifest_contents = fs::read_to_string(&source_manifest_path)?;
    let source_manifest: toml::Value = toml::from_str(&source_manifest_contents)
        .map_err(|error| boxed_err(format!("failed to parse {}: {error}", source_manifest_path.display())))?;

    let workspace = source_manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| boxed_err(format!("{} is missing [workspace]", source_manifest_path.display())))?;
    let source_dependencies =
        workspace.get("dependencies").and_then(toml::Value::as_table).ok_or_else(|| {
            boxed_err(format!("{} is missing [workspace.dependencies]", source_manifest_path.display()))
        })?;

    let mut dependency_names = BTreeSet::new();
    for member in member_dirs {
        let manifest_path = staged_keyos_root.join(member).join("Cargo.toml");
        collect_workspace_dependency_names_from_manifest(&manifest_path, &mut dependency_names)?;
    }

    let mut workspace_dependencies = toml::map::Map::new();
    for dependency_name in dependency_names {
        if let Some(local_spec) =
            local_staged_workspace_dependency_override(&dependency_name, staged_keyos_root)
        {
            workspace_dependencies.insert(dependency_name, local_spec);
            continue;
        }

        let Some(spec) = source_dependencies.get(&dependency_name) else {
            return Err(boxed_err(format!(
                "missing workspace dependency '{dependency_name}' in {}",
                source_manifest_path.display()
            )));
        };
        workspace_dependencies.insert(dependency_name, spec.clone());
    }

    let resolver = workspace.get("resolver").and_then(toml::Value::as_str).unwrap_or("2").to_string();

    let mut workspace_table = toml::map::Map::new();
    workspace_table.insert("resolver".to_string(), toml::Value::String(resolver));
    workspace_table.insert(
        "members".to_string(),
        toml::Value::Array(member_dirs.iter().map(|member| toml::Value::String(member.clone())).collect()),
    );
    workspace_table.insert("dependencies".to_string(), toml::Value::Table(workspace_dependencies));

    let mut document = toml::map::Map::new();
    document.insert("workspace".to_string(), toml::Value::Table(workspace_table));

    toml::to_string(&toml::Value::Table(document))
        .map_err(|error| boxed_err(format!("failed to render staged KeyOS workspace: {error}")))
}

fn collect_workspace_dependency_names_from_manifest(
    manifest_path: &Path,
    names: &mut BTreeSet<String>,
) -> Result<()> {
    let contents = fs::read_to_string(manifest_path)?;
    let manifest: toml::Value = toml::from_str(&contents)
        .map_err(|error| boxed_err(format!("failed to parse {}: {error}", manifest_path.display())))?;
    collect_workspace_dependency_names(&manifest, names);
    Ok(())
}

fn collect_workspace_dependency_names(value: &toml::Value, names: &mut BTreeSet<String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                if let toml::Value::Table(spec) = child {
                    if spec.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                        names.insert(key.clone());
                    }
                }
                collect_workspace_dependency_names(child, names);
            }
        }
        toml::Value::Array(values) => {
            for value in values {
                collect_workspace_dependency_names(value, names);
            }
        }
        _ => {}
    }
}

fn local_staged_workspace_dependency_override(
    dependency_name: &str,
    staged_keyos_root: &Path,
) -> Option<toml::Value> {
    let relative_path = match dependency_name {
        "i-slint-common" => "../slint/internal/common",
        "i-slint-compiler" => "../slint/internal/compiler",
        "i-slint-core" => "../slint/internal/core",
        "slint" => "../slint/api/rs/slint",
        "slint-build" => "../slint/api/rs/build",
        _ => return None,
    };

    let candidate = staged_keyos_root.join(relative_path);
    if !candidate.exists() {
        return None;
    }

    let mut table = toml::map::Map::new();
    table.insert("path".to_string(), toml::Value::String(relative_path.to_string()));

    if matches!(dependency_name, "i-slint-core" | "slint") {
        table.insert("default-features".to_string(), toml::Value::Boolean(false));
    }

    Some(toml::Value::Table(table))
}

fn build_docs(
    root: &Path,
    stage_dir: &Path,
    config: &Config,
    resolver: &SourceResolver,
    verbose: bool,
) -> Result<()> {
    let docs_dir = stage_dir.join("docs");
    let guide_dir = docs_dir.join("guide");
    let api_dir = docs_dir.join("api");
    let guide_source = root.join(&config.docs.guide_source);
    let mdbook_source = guide_source.join("book.toml");

    util::ensure_clean_dir(&guide_dir)?;
    util::ensure_clean_dir(&api_dir)?;

    if mdbook_source.exists() && util::command_exists("mdbook") {
        let mut command = Command::new("mdbook");
        command.arg("build").arg(&guide_source).arg("--dest-dir").arg(&guide_dir);
        util::run_command(&mut command, verbose)?;
    } else {
        util::copy_dir_all(&guide_source, &guide_dir.join("src"))?;
    }

    // Keep the SDK's API docs identical to the release produced by `just docs`: invoke this
    // checkout's generator and configuration, while allowing KEYOS_DIR to supply the KeyOS crate
    // sources. An override checkout may predate docs-api or expose a different SDK crate set.
    let generator_root = docs_generator_root(root)?;
    // Keep the lock from before the child starts until its output is copied to
    // this package stage. The child recognizes this private protocol variable
    // and therefore does not deadlock trying to take the same lock.
    let _docs_bundle_lock = acquire_docs_bundle_lock(&generator_root)?;
    let keyos_root = resolver.keyos_root();
    let mut command = docs_generator_command(&generator_root, keyos_root, &config.sdk.keyos_version);
    util::run_command(&mut command, verbose)?;

    let generated_docs = generator_root.join(DOCS_BUNDLE_OUTPUT_PATH);
    if !generated_docs.join("index.html").is_file() {
        return Err(boxed_err(format!(
            "docs-api did not produce {}",
            generated_docs.join("index.html").display()
        )));
    }
    util::copy_dir_contents(&generated_docs, &api_dir)?;

    util::copy_file_if_exists(&root.join("AGENTS.md"), &docs_dir.join("AGENTS.md"))?;
    Ok(())
}

fn acquire_docs_bundle_lock(generator_root: &Path) -> Result<fs::File> {
    let path = generator_root.join(DOCS_BUNDLE_LOCK_PATH);
    let parent = path.parent().ok_or_else(|| boxed_err("docs bundle lock has no parent directory"))?;
    fs::create_dir_all(parent).map_err(|error| {
        boxed_err(format!("could not create docs bundle lock directory {}: {error}", parent.display()))
    })?;
    let file =
        OpenOptions::new().create(true).read(true).write(true).open(&path).map_err(|error| {
            boxed_err(format!("could not open docs bundle lock {}: {error}", path.display()))
        })?;
    file.lock()
        .map_err(|error| boxed_err(format!("could not lock docs bundle {}: {error}", path.display())))?;
    Ok(file)
}

fn docs_generator_root(sdk_root: &Path) -> Result<PathBuf> {
    let root =
        sdk_root.parent().ok_or_else(|| boxed_err("SDK workspace root has no parent KeyOS checkout"))?;
    if !root.join("xtask/Cargo.toml").is_file() {
        return Err(boxed_err(format!(
            "SDK docs generator is missing from the current KeyOS checkout: {}",
            root.join("xtask/Cargo.toml").display()
        )));
    }
    Ok(root.to_path_buf())
}

fn docs_generator_command(generator_root: &Path, source_root: &Path, keyos_version: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .arg("xtask")
        .arg("docs-api")
        .arg("--keyos-version")
        .arg(keyos_version)
        .arg("--source-root")
        .arg(source_root)
        .current_dir(generator_root)
        .env(DOCS_BUNDLE_LOCK_HELD_ENV, "1");
    command
}

fn render_manifest(
    root: &Path,
    stage_dir: &Path,
    target: &str,
    config: &Config,
    resolver: &SourceResolver,
    release: bool,
    skip_simulator: bool,
) -> Result<String> {
    let mut lines = Vec::new();
    let copy_entries = config.expanded_copy_entries();
    lines.push("[sdk]".to_string());
    lines.push(format!("version = \"{}\"", config.sdk.version));
    lines.push(format!("api_version = \"{}\"", config.sdk.api_version));
    lines.push(format!("keyos_version = \"{}\"", config.sdk.keyos_version));
    lines.push(format!("target = \"{}\"", target));
    lines.push("nix_required = true".to_string());
    lines.push(String::new());

    lines.push("[build]".to_string());
    lines.push(format!("profile = \"{}\"", if release { "release" } else { "debug" }));
    lines.push(format!("host = \"{}\"", host_triple()));
    if let Some(commit) = git_revision(root) {
        lines.push(format!("workspace_commit = \"{}\"", commit));
    }
    if let Some(is_dirty) = git_dirty(root) {
        lines.push(format!("workspace_dirty = {}", is_dirty));
    }
    lines.push(String::new());

    for entry in &config.compile {
        if skip_simulator && entry.name == "simulator" {
            continue;
        }

        let binary_path = stage_dir.join("bin").join(&entry.binary);
        if !binary_path.exists() {
            continue;
        }

        lines.push("[[binaries]]".to_string());
        lines.push(format!("name = \"{}\"", entry.name));
        lines.push(format!("artifact = \"{}\"", entry.artifact.as_deref().unwrap_or(&entry.binary)));
        lines.push(format!("path = \"bin/{}\"", entry.binary));
        lines.push(format!("sha256 = \"{}\"", util::sha256(&binary_path)?));
        lines.push(String::new());
    }

    for entry in &copy_entries {
        let source = resolver.resolve_source(root, &entry.source);
        if !source.exists() && entry.optional {
            continue;
        }

        lines.push("[[sources]]".to_string());
        lines.push(format!("source = \"{}\"", entry.source));
        lines.push(format!("dest = \"{}\"", entry.dest));
        lines.push(format!("optional = {}", entry.optional));
        lines.push(String::new());
    }

    for (name, submodule) in &config.submodules {
        let Some(resolved) = resolver.submodule(name) else {
            continue;
        };

        let resolved_path = resolved
            .path()
            .strip_prefix(root)
            .map(|value| value.display().to_string())
            .unwrap_or_else(|_| resolved.path().display().to_string());
        lines.push(format!("[submodules.{name}]"));
        lines.push(format!("repo = \"{}\"", submodule.repo));
        lines.push(format!("ref = \"{}\"", submodule.r#ref));
        lines.push(format!("resolved_path = \"{}\"", resolved_path));
        if let Some(commit) = git_revision(resolved.path()) {
            lines.push(format!("commit = \"{}\"", commit));
        }
        lines.push(format!(
            "resolved_from = \"{}\"",
            match resolved.resolved_from() {
                ResolvedFrom::Submodule => "submodule",
                ResolvedFrom::Override => "override",
            }
        ));
        lines.push(String::new());
    }

    Ok(lines.join("\n"))
}

fn render_common_manifest(root: &Path, config: &Config, release: bool) -> String {
    let mut lines = vec![
        "[sdk]".to_string(),
        format!("version = \"{}\"", config.sdk.version),
        format!("keyos_version = \"{}\"", config.sdk.keyos_version),
        "kind = \"common\"".to_string(),
        String::new(),
        "[build]".to_string(),
        format!("profile = \"{}\"", if release { "release" } else { "debug" }),
    ];
    if let Some(commit) = git_revision(root) {
        lines.push(format!("workspace_commit = \"{}\"", commit));
    }
    if let Some(is_dirty) = git_dirty(root) {
        lines.push(format!("workspace_dirty = {is_dirty}"));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn host_triple() -> String {
    if let Ok(output) = util::capture_command(Command::new("rustc").arg("-vV")) {
        for line in output.lines() {
            if let Some(value) = line.strip_prefix("host: ") {
                return value.trim().to_string();
            }
        }
    }

    std::env::var("HOST").unwrap_or_else(|_| format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS))
}

fn git_revision(path: &Path) -> Option<String> {
    util::capture_command(Command::new("git").arg("-C").arg(path).arg("rev-parse").arg("HEAD")).ok()
}

fn git_dirty(path: &Path) -> Option<bool> {
    util::capture_command(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("status")
            .arg("--porcelain")
            .arg("--untracked-files=normal"),
    )
    .ok()
    .map(|output| !output.trim().is_empty())
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    iter.next().ok_or_else(|| boxed_err(format!("missing value for {flag}")))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::fs::OpenOptions;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{
        acquire_docs_bundle_lock, cargo_target_linker_env, docs_generator_command,
        ensure_release_sources_are_pinned, git_dirty, is_strippable_binary_header,
        keyos_slint_pin_from_manifest_and_lock, local_staged_workspace_dependency_override, nix_shell_active,
        parse_git_source_commit, parse_scalar_error_enum, parse_toolchain_channel, prune_nested_member_dirs,
        render_staged_keyos_workspace_manifest, render_staged_slint_workspace_manifest,
        rust_toolchain_channel, should_disable_libusb_pkg_config_for_entry, should_scan_for_keyos_member,
        should_stage_simulator_for_target, should_strip_packaged_binaries, stage_bt_error_placeholder,
        stage_cargo_package_snapshot, stage_shared_ui_artifact, stage_slint_sdk_snapshot,
        strip_program_candidates, validate_release_slint_source, validate_release_slint_source_hash,
        verify_common_stage, verify_staged_path_dependencies, verify_target_stage, BuildArgs,
        CommonBuildArgs, ScalarEnumVariant, SmokeCheckArgs, SourceOverrides, StageDirLock,
        BT_PLACEHOLDER_DESTINATION, DOCS_BUNDLE_LOCK_HELD_ENV, DOCS_BUNDLE_LOCK_PATH,
    };
    use crate::config::{load, workspace_root};

    #[test]
    fn stage_dir_lock_blocks_concurrent_holders() {
        let temp = tempfile::tempdir().unwrap();
        let output_dir = temp.path();
        let stage_root = output_dir.join(".stage");

        let first = StageDirLock::acquire(output_dir, &stage_root).expect("first lock should succeed");
        let second = StageDirLock::acquire(output_dir, &stage_root);
        assert!(second.is_err(), "second lock acquisition should fail while first is held");
        let err = second.unwrap_err().to_string();
        assert!(err.contains("another xtask invocation"), "expected lock-contention message, got: {err}");

        // Releasing the first lock should let a new caller succeed.
        drop(first);
        let third = StageDirLock::acquire(output_dir, &stage_root)
            .expect("lock should be reacquirable after release");
        drop(third);
    }

    #[test]
    fn docs_generator_uses_current_checkout_with_resolved_keyos_sources() {
        let generator_root = Path::new("/current/keyos");
        let source_root = Path::new("/overrides/keyos");
        let command = docs_generator_command(generator_root, source_root, "1.4.0-beta3");
        let args = command.get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>();

        assert_eq!(command.get_current_dir(), Some(generator_root));
        assert_eq!(
            args,
            ["xtask", "docs-api", "--keyos-version", "1.4.0-beta3", "--source-root", "/overrides/keyos"]
        );
        assert!(command.get_envs().any(|(name, value)| name.to_string_lossy() == DOCS_BUNDLE_LOCK_HELD_ENV
            && value.is_some_and(|value| value.to_string_lossy() == "1")));
    }

    #[test]
    fn docs_bundle_lock_blocks_copy_until_generation_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let first = acquire_docs_bundle_lock(temp.path()).unwrap();
        let second =
            OpenOptions::new().read(true).write(true).open(temp.path().join(DOCS_BUNDLE_LOCK_PATH)).unwrap();

        assert!(matches!(second.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        first.unlock().unwrap();
        second.try_lock().unwrap();
        second.unlock().unwrap();
    }

    #[test]
    fn cargo_target_linker_env_matches_cargo_convention() {
        assert_eq!(
            cargo_target_linker_env("aarch64-unknown-linux-musl"),
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER"
        );
    }

    #[test]
    fn libusb_pkg_config_is_disabled_only_for_cross_log_viewer() {
        assert!(should_disable_libusb_pkg_config_for_entry(
            "keyos-log-viewer",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu"
        ));
        assert!(!should_disable_libusb_pkg_config_for_entry(
            "passport-drive",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu"
        ));
        assert!(!should_disable_libusb_pkg_config_for_entry(
            "keyos-log-viewer",
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu"
        ));
        assert!(!should_disable_libusb_pkg_config_for_entry(
            "keyos-log-viewer",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin"
        ));
    }

    #[test]
    fn smoke_check_args_parse_sign_and_overrides() {
        let parsed = SmokeCheckArgs::parse(vec![
            "--keyos-dir".into(),
            "/tmp/keyos".into(),
            "--slint-dir".into(),
            "/tmp/slint".into(),
            "--sign".into(),
            "--sign-key".into(),
            "release@example.com".into(),
            "--verbose".into(),
        ])
        .unwrap();

        assert_eq!(parsed.source_overrides.get("keyos").map(PathBuf::as_path), Some(Path::new("/tmp/keyos")));
        assert_eq!(parsed.source_overrides.get("slint").map(PathBuf::as_path), Some(Path::new("/tmp/slint")));
        assert!(parsed.sign);
        assert_eq!(parsed.sign_key.as_deref(), Some("release@example.com"));
        assert!(parsed.verbose);
    }

    #[test]
    fn build_args_parse_package_and_sign() {
        let parsed = BuildArgs::parse(vec![
            "--target".into(),
            "aarch64-apple-darwin".into(),
            "--release".into(),
            "--package".into(),
            "--sign".into(),
            "--sign-key".into(),
            "release@example.com".into(),
            "--verbose".into(),
        ])
        .unwrap();

        assert_eq!(parsed.targets, vec!["aarch64-apple-darwin".to_string()]);
        assert!(parsed.release);
        assert!(parsed.package);
        assert!(parsed.sign);
        assert_eq!(parsed.sign_key.as_deref(), Some("release@example.com"));
        assert!(parsed.verbose);
    }

    #[test]
    fn common_build_args_are_docs_and_package_only() {
        let parsed = CommonBuildArgs::parse(vec![
            "--release".into(),
            "--package".into(),
            "--output-dir".into(),
            "docs-dist".into(),
            "--verbose".into(),
        ])
        .unwrap();

        assert!(parsed.release);
        assert!(parsed.package);
        assert_eq!(parsed.output_dir, PathBuf::from("docs-dist"));
        assert!(parsed.verbose);
        assert!(CommonBuildArgs::parse(vec!["--target".into(), "linux-all".into()]).is_err());
    }

    #[test]
    fn release_builds_reject_local_source_overrides() {
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        let mut explicit = SourceOverrides::new();
        explicit.insert("keyos".into(), PathBuf::from("/tmp/keyos"));
        assert!(ensure_release_sources_are_pinned(true, &explicit, &explicit, &config).is_err());

        let local_slint = tempfile::tempdir().unwrap();
        let mut environment = SourceOverrides::new();
        environment.insert("slint".into(), local_slint.path().to_path_buf());
        let error = ensure_release_sources_are_pinned(true, &SourceOverrides::new(), &environment, &config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is mutable"), "unexpected error: {error}");

        environment.insert("keyos".into(), PathBuf::from("/tmp/keyos"));
        assert!(
            ensure_release_sources_are_pinned(true, &SourceOverrides::new(), &environment, &config).is_err()
        );

        assert!(ensure_release_sources_are_pinned(
            true,
            &SourceOverrides::new(),
            &SourceOverrides::new(),
            &config
        )
        .is_err());
        assert!(ensure_release_sources_are_pinned(false, &explicit, &explicit, &config).is_ok());
    }

    #[test]
    fn release_slint_source_hash_must_match_configuration() {
        let source = Path::new("/nix/store/pinned-slint");
        let expected = "sha256-pinned";
        assert!(validate_release_slint_source_hash(source, expected, expected).is_ok());

        let error =
            validate_release_slint_source_hash(source, expected, "sha256-other").unwrap_err().to_string();
        assert!(error.contains("expected sha256-pinned"), "unexpected error: {error}");
    }

    #[test]
    fn release_slint_source_accepts_configured_nix_source() {
        let Some(source) = env::var_os("FOUNDATION_PINNED_SLINT_DIR") else {
            return;
        };
        let config = load(&workspace_root().join("sdk-build.toml")).unwrap();
        let expected_hash = &config.submodules.get("slint").unwrap().source_hash;

        validate_release_slint_source(Path::new(&source), expected_hash).unwrap();
    }

    #[test]
    fn packaged_or_release_builds_strip_staged_binaries() {
        let mut args = BuildArgs::default();
        assert!(!should_strip_packaged_binaries(&args));

        args.package = true;
        assert!(should_strip_packaged_binaries(&args));

        args.package = false;
        args.release = true;
        assert!(should_strip_packaged_binaries(&args));
    }

    #[test]
    fn build_manifest_marks_untracked_files_dirty() {
        let temp = tempfile::tempdir().unwrap();
        assert!(Command::new("git").arg("init").arg("--quiet").arg(temp.path()).status().unwrap().success());
        assert_eq!(git_dirty(temp.path()), Some(false));

        fs::write(temp.path().join("untracked.txt"), "release input").unwrap();
        assert_eq!(git_dirty(temp.path()), Some(true));
    }

    #[test]
    fn native_target_strip_falls_back_to_host_tools() {
        assert_eq!(
            strip_program_candidates(Some("aarch64-unknown-linux-musl-strip"), true),
            ["aarch64-unknown-linux-musl-strip", "strip", "llvm-strip"]
        );
        assert_eq!(
            strip_program_candidates(Some("aarch64-unknown-linux-musl-strip"), false),
            ["aarch64-unknown-linux-musl-strip"]
        );
    }

    #[test]
    fn simulator_runtime_is_only_staged_for_host_target() {
        assert!(should_stage_simulator_for_target(
            "x86_64-unknown-linux-gnu",
            false,
            "x86_64-unknown-linux-gnu"
        ));
        assert!(!should_stage_simulator_for_target(
            "aarch64-apple-darwin",
            false,
            "x86_64-unknown-linux-gnu"
        ));
        assert!(!should_stage_simulator_for_target(
            "x86_64-unknown-linux-gnu",
            true,
            "x86_64-unknown-linux-gnu"
        ));
    }

    #[test]
    fn strippable_binary_header_matches_elf_and_macho() {
        assert!(is_strippable_binary_header([0x7f, b'E', b'L', b'F']));
        assert!(is_strippable_binary_header([0xcf, 0xfa, 0xed, 0xfe]));
        assert!(is_strippable_binary_header([0xca, 0xfe, 0xba, 0xbf]));
        assert!(!is_strippable_binary_header([b'#', b'!', b'/', b'u']));
        assert!(!is_strippable_binary_header([0, 1, 2, 3]));
    }

    #[test]
    fn verify_target_stage_requires_simulator_for_full_bundle() {
        let (_stage_guard, stage_dir) = temp_stage_dir();
        fs::create_dir_all(stage_dir.join("bin")).unwrap();

        for path in [
            stage_dir.join("bin").join("foundation"),
            stage_dir.join("bin").join("foundation-asset-tool"),
            stage_dir.join("bin").join("fatfs-image"),
            stage_dir.join("bin").join("foundation-slint-viewer"),
            stage_dir.join("bin").join("foundation-keyos-log-viewer"),
            stage_dir.join("bin").join("foundation-passport-drive"),
            stage_dir.join("bin").join("foundation-theme-editor"),
            stage_dir.join("bin").join("cosign2"),
            stage_dir.join("manifest.toml"),
        ] {
            fs::write(path, "").unwrap();
        }

        let error = verify_target_stage(&stage_dir, false).unwrap_err();
        assert!(error.to_string().contains("foundation-simulator"));
    }

    #[test]
    fn verify_staged_path_dependencies_names_every_missing_crate() {
        let (_stage_guard, stage_dir) = temp_stage_dir();
        let keyos = stage_dir.join("lib").join("keyos");
        fs::create_dir_all(keyos.join("api").join("nfc")).unwrap();
        fs::write(keyos.join("Cargo.toml"), "[workspace.dependencies.gpio]\npath = \"api/gpio\"\n").unwrap();
        fs::write(
            keyos.join("api").join("nfc").join("Cargo.toml"),
            "[dependencies]\nserver = { path = \"../../server\" }\n# path = \"commented/out\"\n",
        )
        .unwrap();

        let error = verify_staged_path_dependencies(&keyos).unwrap_err().to_string();
        assert!(error.contains("api/gpio"), "{error}");
        assert!(error.contains("../../server"), "{error}");
        assert!(!error.contains("commented/out"), "{error}");

        fs::create_dir_all(keyos.join("api").join("gpio")).unwrap();
        fs::create_dir_all(keyos.join("server")).unwrap();
        verify_staged_path_dependencies(&keyos).unwrap();
    }

    #[test]
    fn verify_common_stage_requires_shared_layout() {
        let (_stage_guard, stage_dir) = temp_stage_dir();
        fs::create_dir_all(stage_dir.join("docs").join("guide").join("src")).unwrap();
        fs::create_dir_all(stage_dir.join("docs").join("api")).unwrap();
        for tool_dir in [".agents", ".claude"] {
            let skill = stage_dir.join(tool_dir).join("skills").join("foundation-cli");
            fs::create_dir_all(&skill).unwrap();
            fs::write(skill.join("SKILL.md"), "").unwrap();
        }
        fs::create_dir_all(stage_dir.join("lib").join("keyos")).unwrap();
        fs::create_dir_all(stage_dir.join(BT_PLACEHOLDER_DESTINATION).join("src")).unwrap();
        fs::create_dir_all(stage_dir.join("lib").join("keyos").join("ui2").join("components")).unwrap();
        fs::create_dir_all(
            stage_dir
                .join("lib")
                .join("keyos")
                .join("sdk")
                .join("crates")
                .join("foundation-themes")
                .join("src"),
        )
        .unwrap();
        fs::create_dir_all(
            stage_dir
                .join("lib")
                .join("keyos")
                .join("sdk")
                .join("crates")
                .join("foundation-themes")
                .join("themes"),
        )
        .unwrap();
        fs::create_dir_all(stage_dir.join("ui").join("ui")).unwrap();
        fs::create_dir_all(stage_dir.join("resources").join("icons")).unwrap();
        fs::create_dir_all(stage_dir.join("lib").join("keyos").join("utils").join("fiat-symbols")).unwrap();
        fs::create_dir_all(stage_dir.join("lib").join("keyos").join("utils").join("localizer-codegen"))
            .unwrap();

        for path in [
            stage_dir.join("manifest.toml"),
            stage_dir.join("lib").join("keyos").join("Cargo.toml"),
            stage_dir.join(BT_PLACEHOLDER_DESTINATION).join("Cargo.toml"),
            stage_dir.join(BT_PLACEHOLDER_DESTINATION).join("src").join("error.rs"),
            stage_dir.join("lib").join("keyos").join("ui2").join("components").join("Cargo.toml"),
            stage_dir
                .join("lib")
                .join("keyos")
                .join("sdk")
                .join("crates")
                .join("foundation-themes")
                .join("Cargo.toml"),
            stage_dir
                .join("lib")
                .join("keyos")
                .join("sdk")
                .join("crates")
                .join("foundation-themes")
                .join("src")
                .join("build.rs"),
            stage_dir
                .join("lib")
                .join("keyos")
                .join("sdk")
                .join("crates")
                .join("foundation-themes")
                .join("themes")
                .join("base_theme.json"),
            stage_dir.join("ui").join("ui").join("theme.slint"),
            stage_dir.join("lib").join("keyos").join("utils").join("fiat-symbols").join("Cargo.toml"),
            stage_dir.join("lib").join("keyos").join("utils").join("localizer-codegen").join("Cargo.toml"),
            stage_dir.join("resources").join("icons").join("loader.svg"),
            stage_dir.join("docs").join("guide").join("src").join("foundation-cli.md"),
            stage_dir.join("flake.nix"),
            stage_dir.join("setup.sh"),
        ] {
            fs::write(path, "").unwrap();
        }

        assert!(verify_common_stage(&stage_dir, false).is_ok());

        // A copy that did not follow the .claude/skills symlink stages the skills
        // under .agents alone, which no agent tool reads.
        fs::remove_dir_all(stage_dir.join(".claude")).unwrap();
        assert!(verify_common_stage(&stage_dir, false).is_err());
    }

    #[test]
    fn shared_ui_is_generated_into_artifact_stage() {
        let root = workspace_root();
        let keyos_root = root.parent().expect("sdk workspace has KeyOS parent");
        let (_stage_guard, stage_dir) = temp_stage_dir();

        stage_shared_ui_artifact(keyos_root, &stage_dir, false).unwrap();

        assert!(stage_dir.join("ui").join("ui").join("theme.slint").exists());
        assert!(stage_dir.join("lib").join("keyos").join("ui").join("ui").join("theme.slint").exists());
        assert!(stage_dir.join("resources").join("icons").join("loader.svg").exists());
        // Theme JSON is shipped by the foundation-themes crate copy (the
        // cargo_package snapshot, covered by
        // stage_cargo_package_snapshot_omits_target_directory), not by the
        // shared-UI staging here.
    }

    #[test]
    fn parse_toolchain_channel_reads_rust_toolchain_toml() {
        let (_stage_guard, stage_dir) = temp_stage_dir();
        let toolchain = stage_dir.join("rust-toolchain.toml");
        fs::write(&toolchain, "[toolchain]\nchannel = \"nightly-2026-04-11\"\n").unwrap();

        assert_eq!(parse_toolchain_channel(&toolchain).as_deref(), Some("nightly-2026-04-11"));
    }

    #[test]
    fn rust_toolchain_channel_walks_parent_directories() {
        let (_root_guard, root) = temp_stage_dir();
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("rust-toolchain.toml"), "[toolchain]\nchannel = \"nightly-2026-04-11\"\n")
            .unwrap();

        assert_eq!(rust_toolchain_channel(&nested).as_deref(), Some("nightly-2026-04-11"));
    }

    #[test]
    fn nix_shell_active_detects_foundation_development_shell() {
        let previous = env::var_os("FOUNDATION_DEVELOP_SHELL");
        env::set_var("FOUNDATION_DEVELOP_SHELL", "1");

        assert!(nix_shell_active());

        if let Some(value) = previous {
            env::set_var("FOUNDATION_DEVELOP_SHELL", value);
        } else {
            env::remove_var("FOUNDATION_DEVELOP_SHELL");
        }
    }

    #[test]
    fn parse_git_source_commit_extracts_locked_revision() {
        let source =
            "git+https://github.com/Foundation-Devices/slint.git?tag=v1.12.1-foundation10#687e39174c111f001ce8b0c7eeeedbc6ab05be48";
        assert_eq!(
            parse_git_source_commit(source).as_deref(),
            Some("687e39174c111f001ce8b0c7eeeedbc6ab05be48")
        );
    }

    #[test]
    fn keyos_slint_pin_reads_tag_and_commit() {
        let manifest = r#"
[workspace.dependencies]
slint = { git = "https://github.com/Foundation-Devices/slint.git", tag = "v1.12.1-foundation10" }
"#;
        let lockfile = r#"
version = 3

[[package]]
name = "slint"
version = "1.12.1"
source = "git+https://github.com/Foundation-Devices/slint.git?tag=v1.12.1-foundation10#687e39174c111f001ce8b0c7eeeedbc6ab05be48"
"#;

        let pin = keyos_slint_pin_from_manifest_and_lock(manifest, lockfile).unwrap();
        assert_eq!(pin.tag, "v1.12.1-foundation10");
        assert_eq!(pin.commit, "687e39174c111f001ce8b0c7eeeedbc6ab05be48");
    }

    #[test]
    fn prune_nested_member_dirs_removes_redundant_children() {
        let members = prune_nested_member_dirs(vec![
            "helper_crates/const-field-offset".to_string(),
            "helper_crates/const-field-offset/macro".to_string(),
            "internal/compiler".to_string(),
            "internal/compiler/parser-test-macro".to_string(),
            "internal/core".to_string(),
        ]);

        assert_eq!(
            members,
            vec![
                "helper_crates/const-field-offset".to_string(),
                "internal/compiler".to_string(),
                "internal/core".to_string(),
            ]
        );
    }

    #[test]
    fn staged_keyos_workspace_manifest_includes_member_deps() {
        let keyos_root = workspace_root().parent().unwrap().to_path_buf();
        let manifest = render_staged_keyos_workspace_manifest(
            &keyos_root,
            &keyos_root,
            &["server".to_string(), "server/macro".to_string(), "ui2/components".to_string()],
        )
        .unwrap();

        let parsed: toml::Value = toml::from_str(&manifest).unwrap();
        let workspace = parsed.get("workspace").and_then(toml::Value::as_table).unwrap();
        let members = workspace
            .get("members")
            .and_then(toml::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>();
        assert_eq!(members, vec!["server", "server/macro", "ui2/components"]);

        let dependencies = workspace.get("dependencies").and_then(toml::Value::as_table).unwrap();
        for dependency in ["defer", "log", "log-server", "rkyv", "whence", "xous-names"] {
            assert!(dependencies.contains_key(dependency));
        }
        assert!(dependencies.contains_key("slint"));
    }

    #[test]
    fn stages_only_the_wire_compatible_bt_error_placeholder() {
        let keyos_root = workspace_root().parent().unwrap().to_path_buf();
        let stage = tempfile::tempdir().unwrap();
        stage_bt_error_placeholder(&keyos_root, stage.path()).unwrap();

        let placeholder = stage.path().join("lib/keyos/api/bt");
        assert!(placeholder.join("Cargo.toml").is_file());
        assert!(placeholder.join("src/lib.rs").is_file());
        assert!(!placeholder.join("src/messages.rs").exists());
        assert_eq!(
            fs::read_to_string(placeholder.join("src/error.rs")).unwrap(),
            fs::read_to_string(keyos_root.join("api/bt/src/error.rs")).unwrap()
        );

        let real_gpio = fs::read_to_string(keyos_root.join("api/gpio/src/lib.rs")).unwrap();
        let real_spi = fs::read_to_string(keyos_root.join("api/spi/src/error.rs")).unwrap();
        let placeholder_lib = fs::read_to_string(placeholder.join("src/lib.rs")).unwrap();
        assert_eq!(
            enum_variant_names(&real_gpio, "pub enum GpioApiError {"),
            enum_variant_names(&placeholder_lib, "pub enum GpioApiError {")
        );
        assert_eq!(
            enum_variant_names(&real_spi, "pub enum SpiError {"),
            enum_variant_names(&placeholder_lib, "pub enum SpiError {")
        );
        assert!(
            placeholder_lib.contains("12 => Self::InvalidWordSize,\n            _ => Self::InternalError,")
        );
        assert!(!placeholder_lib.contains("BluetoothApi"));
        assert!(!placeholder_lib.contains("use_api"));
    }

    #[test]
    fn bt_placeholder_uses_payload_enums_from_the_selected_keyos_root() {
        let keyos = tempfile::tempdir().unwrap();
        let gpio = keyos.path().join("api/gpio/src/lib.rs");
        let spi = keyos.path().join("api/spi/src/error.rs");
        let bluetooth_error = keyos.path().join("api/bt/src/error.rs");
        fs::create_dir_all(gpio.parent().unwrap()).unwrap();
        fs::create_dir_all(spi.parent().unwrap()).unwrap();
        fs::create_dir_all(bluetooth_error.parent().unwrap()).unwrap();
        fs::write(&gpio, "pub enum GpioApiError { SelectedGpio = 7, InternalError = 9, }").unwrap();
        fs::write(&spi, "pub enum SpiError { SelectedSpi = 3, InternalError, }").unwrap();
        fs::write(&bluetooth_error, "pub enum BluetoothError {} ").unwrap();
        let stage = tempfile::tempdir().unwrap();
        stage_bt_error_placeholder(keyos.path(), stage.path()).unwrap();
        let rendered =
            fs::read_to_string(stage.path().join(BT_PLACEHOLDER_DESTINATION).join("src/lib.rs")).unwrap();

        assert!(rendered.contains("SelectedGpio = 7"));
        assert!(rendered.contains("7 => Self::SelectedGpio"));
        assert!(rendered.contains("SelectedSpi = 3"));
        assert!(rendered.contains("4 => Self::InternalError"));
        assert!(!rendered.contains("AlreadyClaimed"));
    }

    #[test]
    fn bt_placeholder_rejects_unsupported_payload_enum_shapes() {
        let error = parse_scalar_error_enum(
            "pub enum GpioApiError { Structured { field: u32 }, InternalError, }",
            "GpioApiError",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("unsupported non-unit variant"));
    }

    #[test]
    fn bt_placeholder_ignores_comments_when_parsing_payload_enums() {
        let variants = parse_scalar_error_enum(
            "pub enum SpiError {\n    /// Busy, retry later.\n    AlreadyClaimed = 1,\n    /* Reserved, for a future protocol. */\n    InternalError,\n}",
            "SpiError",
        )
        .unwrap();

        assert_eq!(
            variants,
            vec![
                ScalarEnumVariant { name: "AlreadyClaimed".to_owned(), discriminant: 1 },
                ScalarEnumVariant { name: "InternalError".to_owned(), discriminant: 2 },
            ]
        );
    }

    #[test]
    #[ignore = "spawns nested Cargo checks for staged Quantum Link and bt"]
    fn staged_quantum_link_accepts_bt_placeholder_and_bt_compiles_for_keyos() {
        let keyos_root = workspace_root().parent().unwrap().to_path_buf();
        let stage = tempfile::tempdir().unwrap();
        stage_bt_error_placeholder(&keyos_root, stage.path()).unwrap();
        let staged_keyos_root = stage.path().join("lib/keyos");
        crate::util::copy_dir_all(
            &keyos_root.join("api/quantum-link"),
            &staged_keyos_root.join("api/quantum-link"),
        )
        .unwrap();

        let manifest = render_staged_keyos_workspace_manifest(
            &keyos_root,
            &staged_keyos_root,
            &["api/bt".to_string(), "api/quantum-link".to_string()],
        )
        .unwrap();
        let mut manifest: toml::Value = toml::from_str(&manifest).unwrap();
        let dependencies = manifest
            .get_mut("workspace")
            .and_then(toml::Value::as_table_mut)
            .and_then(|workspace| workspace.get_mut("dependencies"))
            .and_then(toml::Value::as_table_mut)
            .unwrap();
        for (name, path) in [
            ("server", keyos_root.join("server")),
            ("worker", keyos_root.join("worker")),
            ("xous", keyos_root.join("xous/xous-rs")),
        ] {
            dependencies
                .get_mut(name)
                .unwrap()
                .as_table_mut()
                .unwrap()
                .insert("path".to_string(), toml::Value::String(path.to_string_lossy().into_owned()));
        }

        fs::write(staged_keyos_root.join("Cargo.toml"), toml::to_string(&manifest).unwrap()).unwrap();

        for (name, package, target) in
            [("host", "quantum-link", None), ("keyos", "bt", Some("armv7a-unknown-xous-elf"))]
        {
            let mut command = Command::new("cargo");
            command
                .args(["check", "--manifest-path"])
                .arg(staged_keyos_root.join("Cargo.toml"))
                .args(["-p", package])
                .env("CARGO_TARGET_DIR", stage.path().join(format!("target-{name}")));
            if let Some(target) = target {
                command.args(["--target", target]);
            }
            let status = command.status().unwrap();
            assert!(status.success(), "staged {package} failed the {name} Cargo check");
        }
    }

    fn enum_variant_names(source: &str, declaration: &str) -> Vec<String> {
        let body = source
            .split_once(declaration)
            .unwrap_or_else(|| panic!("missing enum declaration {declaration}"))
            .1
            .split_once("\n}")
            .unwrap_or_else(|| panic!("unterminated enum declaration {declaration}"))
            .0;
        body.lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                    return None;
                }
                let name = line.split(['(', '=', ',']).next().unwrap_or_default().trim();
                name.chars()
                    .all(|character| character == '_' || character.is_ascii_alphanumeric())
                    .then(|| name.to_string())
            })
            .collect()
    }

    #[test]
    fn staged_keyos_member_scan_includes_ui2_components_but_not_generated_ui() {
        assert!(should_scan_for_keyos_member("lib/keyos/ui2/components"));
        assert!(should_scan_for_keyos_member("lib/keyos/ui2/components/src"));
        assert!(!should_scan_for_keyos_member("lib/keyos/ui/ui"));
        assert!(!should_scan_for_keyos_member("lib/keyos/ui/ui/theme.slint"));
        assert!(!should_scan_for_keyos_member("lib/keyos/keyos"));
        assert!(!should_scan_for_keyos_member("lib/keyos/simulator"));
    }

    #[test]
    fn staged_workspace_uses_local_slint_paths_when_packaged() {
        let (_root_guard, root) = temp_stage_dir();
        let staged_keyos_root = root.join("lib").join("keyos");
        fs::create_dir_all(root.join("lib").join("slint").join("internal").join("common")).unwrap();
        fs::create_dir_all(root.join("lib").join("slint").join("internal").join("compiler")).unwrap();
        fs::create_dir_all(root.join("lib").join("slint").join("internal").join("core")).unwrap();
        fs::create_dir_all(root.join("lib").join("slint").join("api").join("rs").join("slint")).unwrap();
        fs::create_dir_all(root.join("lib").join("slint").join("api").join("rs").join("build")).unwrap();
        fs::create_dir_all(&staged_keyos_root).unwrap();

        let slint = local_staged_workspace_dependency_override("slint", &staged_keyos_root)
            .and_then(|value| value.as_table().cloned())
            .unwrap();
        assert_eq!(slint.get("path").and_then(toml::Value::as_str), Some("../slint/api/rs/slint"));
        assert_eq!(slint.get("default-features").and_then(toml::Value::as_bool), Some(false));

        let common = local_staged_workspace_dependency_override("i-slint-common", &staged_keyos_root)
            .and_then(|value| value.as_table().cloned())
            .unwrap();
        assert_eq!(common.get("path").and_then(toml::Value::as_str), Some("../slint/internal/common"));
    }

    #[test]
    fn staged_slint_workspace_manifest_filters_missing_members_and_paths() {
        let (_root_guard, root) = temp_stage_dir();
        fs::create_dir_all(root.join("api").join("rs").join("build")).unwrap();
        fs::create_dir_all(root.join("api").join("rs").join("slint")).unwrap();
        fs::create_dir_all(root.join("internal").join("core")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            r#"
[workspace]
members = ["api/rs/build", "api/rs/slint", "internal/core", "examples/gallery"]
default-members = ["api/rs/build", "examples/gallery"]
resolver = "2"

[workspace.dependencies]
i-slint-backend-android-activity = { path = "internal/backends/android-activity" }
slint-build = { path = "api/rs/build" }
slint = { path = "api/rs/slint" }
i-slint-core = { path = "internal/core" }
slint-cpp = { path = "api/cpp" }
serde = "1"

[workspace.package]
edition = "2021"

[profile.release]
lto = true
"#,
        )
        .unwrap();

        let manifest = render_staged_slint_workspace_manifest(
            &root,
            &root,
            &["api/rs/build".to_string(), "api/rs/slint".to_string(), "internal/core".to_string()],
        )
        .unwrap();
        let parsed: toml::Value = toml::from_str(&manifest).unwrap();
        let workspace = parsed.get("workspace").and_then(toml::Value::as_table).unwrap();
        let members = workspace
            .get("members")
            .and_then(toml::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>();
        assert_eq!(members, vec!["api/rs/build", "api/rs/slint", "internal/core"]);

        let default_members = workspace
            .get("default-members")
            .and_then(toml::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>();
        assert_eq!(default_members, vec!["api/rs/build"]);

        let dependencies = workspace.get("dependencies").and_then(toml::Value::as_table).unwrap();
        assert!(dependencies.contains_key("slint-build"));
        assert!(dependencies.contains_key("slint"));
        assert!(dependencies.contains_key("i-slint-core"));
        assert!(dependencies.contains_key("serde"));
        assert!(!dependencies.contains_key("slint-cpp"));
        assert!(!dependencies.contains_key("i-slint-backend-android-activity"));
    }

    #[test]
    fn slint_snapshot_excludes_local_build_artifacts() {
        let (_source_guard, source_root) = temp_stage_dir();
        let (_dest_guard, destination_root) = temp_stage_dir();

        for dir in [
            source_root.join(".cargo"),
            source_root.join("LICENSES"),
            source_root.join("api").join("rs").join("build"),
            source_root.join("api").join("rs").join("slint"),
            source_root.join("api").join("rs").join("macros"),
            source_root.join("helper_crates").join("const-field-offset"),
            source_root.join("helper_crates").join("vtable").join("macro"),
            source_root.join("internal").join("backends").join("android-activity"),
            source_root.join("internal").join("backends").join("linuxkms"),
            source_root.join("internal").join("backends").join("qt"),
            source_root.join("internal").join("backends").join("selector"),
            source_root.join("internal").join("backends").join("winit"),
            source_root.join("internal").join("common"),
            source_root.join("internal").join("compiler").join("parser-test-macro"),
            source_root.join("internal").join("core"),
            source_root.join("internal").join("core-macros"),
            source_root.join("internal").join("interpreter"),
            source_root.join("internal").join("renderers").join("femtovg"),
            source_root.join("internal").join("renderers").join("skia"),
            source_root.join("target").join("junk"),
            source_root.join("docs"),
            source_root.join("examples"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }

        fs::write(source_root.join("Cargo.lock"), "").unwrap();
        fs::write(source_root.join("LICENSE.md"), "").unwrap();
        fs::write(source_root.join(".cargo").join("config.toml"), "").unwrap();
        fs::write(source_root.join("LICENSES").join("MIT.txt"), "").unwrap();
        fs::write(
            source_root.join("Cargo.toml"),
            r#"
[workspace]
members = [
  "api/rs/build",
  "api/rs/macros",
  "api/rs/slint",
  "helper_crates/const-field-offset",
  "helper_crates/vtable",
  "internal/backends/linuxkms",
  "internal/backends/qt",
  "internal/backends/selector",
  "internal/backends/winit",
  "internal/common",
  "internal/compiler",
  "internal/core",
  "internal/core-macros",
  "internal/interpreter",
  "internal/renderers/femtovg",
  "internal/renderers/skia",
]
default-members = ["api/rs/build"]
resolver = "2"

[workspace.dependencies]
i-slint-backend-android-activity = { path = "internal/backends/android-activity" }
slint-build = { path = "api/rs/build" }
slint = { path = "api/rs/slint" }
slint-macros = { path = "api/rs/macros" }
i-slint-common = { path = "internal/common" }
i-slint-compiler = { path = "internal/compiler" }
i-slint-core = { path = "internal/core" }
i-slint-core-macros = { path = "internal/core-macros" }
slint-interpreter = { path = "internal/interpreter" }
i-slint-backend-selector = { path = "internal/backends/selector" }
i-slint-backend-winit = { path = "internal/backends/winit" }
i-slint-backend-qt = { path = "internal/backends/qt" }
i-slint-backend-linuxkms = { path = "internal/backends/linuxkms" }
i-slint-renderer-femtovg = { path = "internal/renderers/femtovg" }
i-slint-renderer-skia = { path = "internal/renderers/skia" }
vtable = { path = "helper_crates/vtable" }
serde = "1"
"#,
        )
        .unwrap();

        let manifests = [
            (
                "api/rs/build/Cargo.toml",
                "[package]\nname = \"slint-build\"\nversion = \"1.0.0\"\n",
            ),
            (
                "api/rs/macros/Cargo.toml",
                "[package]\nname = \"slint-macros\"\nversion = \"1.0.0\"\n",
            ),
            (
                "api/rs/slint/Cargo.toml",
                "[package]\nname = \"slint\"\nversion = \"1.0.0\"\n[dependencies]\nslint-macros = { path = \"../macros\" }\ni-slint-core = { path = \"../../../internal/core\" }\n[target.'cfg(target_os = \"android\")'.dependencies]\ni-slint-backend-android-activity = { workspace = true, optional = true }\n",
            ),
            (
                "helper_crates/const-field-offset/Cargo.toml",
                "[package]\nname = \"const-field-offset\"\nversion = \"1.0.0\"\n",
            ),
            (
                "helper_crates/vtable/Cargo.toml",
                "[package]\nname = \"vtable\"\nversion = \"1.0.0\"\n[dependencies]\nvtable-macro = { path = \"./macro\" }\nconst-field-offset = { path = \"../const-field-offset\" }\n",
            ),
            (
                "helper_crates/vtable/macro/Cargo.toml",
                "[package]\nname = \"vtable-macro\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/common/Cargo.toml",
                "[package]\nname = \"i-slint-common\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/compiler/Cargo.toml",
                "[package]\nname = \"i-slint-compiler\"\nversion = \"1.0.0\"\n[dev-dependencies]\ni-slint-parser-test-macro = { path = \"./parser-test-macro\" }\n",
            ),
            (
                "internal/compiler/parser-test-macro/Cargo.toml",
                "[package]\nname = \"i-slint-parser-test-macro\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/core/Cargo.toml",
                "[package]\nname = \"i-slint-core\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/core-macros/Cargo.toml",
                "[package]\nname = \"i-slint-core-macros\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/interpreter/Cargo.toml",
                "[package]\nname = \"slint-interpreter\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/backends/android-activity/Cargo.toml",
                "[package]\nname = \"i-slint-backend-android-activity\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/backends/selector/Cargo.toml",
                "[package]\nname = \"i-slint-backend-selector\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/backends/winit/Cargo.toml",
                "[package]\nname = \"i-slint-backend-winit\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/backends/qt/Cargo.toml",
                "[package]\nname = \"i-slint-backend-qt\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/backends/linuxkms/Cargo.toml",
                "[package]\nname = \"i-slint-backend-linuxkms\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/renderers/femtovg/Cargo.toml",
                "[package]\nname = \"i-slint-renderer-femtovg\"\nversion = \"1.0.0\"\n",
            ),
            (
                "internal/renderers/skia/Cargo.toml",
                "[package]\nname = \"i-slint-renderer-skia\"\nversion = \"1.0.0\"\n",
            ),
        ];

        for (relative_path, contents) in manifests {
            let path = source_root.join(relative_path);
            fs::write(path, contents).unwrap();
        }
        fs::write(source_root.join("target").join("junk").join("ignored.txt"), "ignored").unwrap();
        fs::write(source_root.join("docs").join("ignored.md"), "ignored").unwrap();
        fs::write(source_root.join("examples").join("ignored.md"), "ignored").unwrap();

        stage_slint_sdk_snapshot(&source_root, &destination_root).unwrap();

        assert!(destination_root.join("Cargo.toml").exists());
        assert!(destination_root.join("Cargo.lock").exists());
        assert!(destination_root.join(".cargo").join("config.toml").exists());
        assert!(destination_root
            .join("internal")
            .join("backends")
            .join("android-activity")
            .join("Cargo.toml")
            .exists());
        assert!(destination_root
            .join("helper_crates")
            .join("vtable")
            .join("macro")
            .join("Cargo.toml")
            .exists());
        assert!(destination_root
            .join("internal")
            .join("compiler")
            .join("parser-test-macro")
            .join("Cargo.toml")
            .exists());
        assert!(!destination_root.join("target").exists());
        assert!(!destination_root.join("docs").exists());
        assert!(!destination_root.join("examples").exists());
        let staged_manifest = fs::read_to_string(destination_root.join("Cargo.toml")).unwrap();
        assert!(staged_manifest.contains("[workspace.dependencies.i-slint-backend-android-activity]"));
    }

    #[test]
    fn stage_cargo_package_snapshot_omits_target_directory() {
        let (_source_guard, source_root) = temp_stage_dir();
        let (_dest_guard, destination_root) = temp_stage_dir();

        fs::create_dir_all(source_root.join("src")).unwrap();
        fs::create_dir_all(source_root.join("themes")).unwrap();
        fs::create_dir_all(source_root.join("target").join("debug")).unwrap();
        fs::write(source_root.join("Cargo.toml"), "[package]\nname = \"foundation-themes\"\n").unwrap();
        fs::write(source_root.join("src").join("lib.rs"), "pub mod build;\n").unwrap();
        fs::write(source_root.join("themes").join("base_theme.json"), "{}").unwrap();
        fs::write(source_root.join("target").join("debug").join("ignored"), "ignored").unwrap();

        stage_cargo_package_snapshot(&source_root, &destination_root).unwrap();

        assert!(destination_root.join("Cargo.toml").exists());
        assert!(destination_root.join("src").join("lib.rs").exists());
        assert!(destination_root.join("themes").join("base_theme.json").exists());
        assert!(!destination_root.join("target").exists());
    }

    fn temp_stage_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }
}

fn parse_override(
    overrides: &mut SourceOverrides,
    name: &str,
    iter: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<()> {
    overrides.insert(name.to_string(), PathBuf::from(next_value(iter, flag)?));
    Ok(())
}
