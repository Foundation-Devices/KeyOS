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
    let output_dir = util::absolute_path(root, &args.output_dir);
    util::ensure_dir(&output_dir)?;

    // Hold a file-based lock for the entire build so two parallel xtask
    // invocations can't clobber each other's stage directories. The lock lives
    // alongside (not inside) `.stage/` so the imminent `ensure_clean_dir` call
    // doesn't wipe it out from under us. Released when `_stage_lock` drops.
    let stage_root = package::stage_root_dir(&output_dir);
    let _stage_lock = StageDirLock::acquire(&output_dir, &stage_root)?;

    util::ensure_clean_dir(&stage_root)?;
    let targets = selected_targets(config, &args.targets)?;
    let mut source_overrides = args.source_overrides.clone();
    submodules::apply_env_overrides(config, &mut source_overrides);
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

    stage_keyos_workspace_root(root, config, &stage_dir, &config.expanded_copy_entries(), resolver)?;
    stage_shared_ui_artifact(resolver.keyos_root(), &stage_dir, args.verbose)?;

    if !args.skip_docs {
        build_docs(root, &stage_dir, "common", config, args.verbose)?;
    }

    util::copy_file(&root.join(SDK_USER_FLAKE_SOURCE), &stage_dir.join("flake.nix"))?;
    util::copy_file_if_exists(&root.join(SDK_USER_FLAKE_LOCK_SOURCE), &stage_dir.join("flake.lock"))?;
    util::copy_file(&root.join("setup.sh"), &stage_dir.join("setup.sh"))?;

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
        compile_entry(root, config, &stage_dir, target, entry, args, resolver)?;
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

fn verify_common_stage(stage_dir: &Path, skip_docs: bool) -> Result<()> {
    let mut required_paths = vec![
        stage_dir.join("lib").join("keyos").join("Cargo.toml"),
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
            .join("default_theme.json"),
        stage_dir.join("lib").join("keyos").join("utils").join("fiat-symbols").join("Cargo.toml"),
        stage_dir.join("resources").join("icons").join("loader.svg"),
        stage_dir.join(".agents").join("skills").join("foundation-cli").join("SKILL.md"),
        stage_dir.join(".agents").join("skills").join("foundation-localize").join("SKILL.md"),
        stage_dir.join(".agents").join("skills").join("foundation-new-page").join("SKILL.md"),
        stage_dir.join(".claude").join("skills").join("foundation-cli").join("SKILL.md"),
        stage_dir.join(".claude").join("skills").join("foundation-localize").join("SKILL.md"),
        stage_dir.join(".claude").join("skills").join("foundation-new-page").join("SKILL.md"),
        stage_dir.join("flake.nix"),
        stage_dir.join("setup.sh"),
    ];

    if !skip_docs {
        required_paths.push(stage_dir.join("docs").join("guide"));
        required_paths.push(stage_dir.join("docs").join("api"));
        required_paths.push(stage_dir.join("docs").join("guide").join("src").join("foundation-cli.md"));
    }

    let missing = required_paths.into_iter().filter(|path| !path.exists()).collect::<Vec<_>>();

    if missing.is_empty() {
        return Ok(());
    }

    Err(boxed_err(format!(
        "staged common SDK content at {} is missing required paths:\n{}",
        stage_dir.display(),
        missing.iter().map(|path| format!("- {}", path.display())).collect::<Vec<_>>().join("\n")
    )))
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

    for crate_path in &config.docs.api_crates {
        match resolve_staged_source(root, resolver, &copy_entries, crate_path) {
            Some(source) => {
                let manifest_path = source.join("Cargo.toml");
                if !manifest_path.exists() {
                    report.warnings.push(format!(
                        "staged API docs source '{}' does not contain Cargo.toml at {}",
                        crate_path,
                        manifest_path.display()
                    ));
                }
            }
            None => report
                .warnings
                .push(format!("staged API docs source '{}' does not map to any [[copy]] entry", crate_path)),
        }
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

fn resolve_staged_source(
    root: &Path,
    resolver: &SourceResolver,
    copies: &[CopyEntry],
    staged_path: &str,
) -> Option<PathBuf> {
    let normalized = staged_path.trim_start_matches("./");

    for entry in copies {
        if normalized == entry.dest || normalized.starts_with(&format!("{}/", entry.dest)) {
            let suffix = normalized.strip_prefix(&entry.dest).unwrap_or_default().trim_start_matches('/');
            let source_root = resolver.resolve_source(root, &entry.source);
            return Some(if suffix.is_empty() { source_root } else { source_root.join(suffix) });
        }
    }

    None
}

fn compile_entry(
    root: &Path,
    config: &Config,
    stage_dir: &Path,
    target: &str,
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

    let mut command = cargo_command_for_manifest(&manifest_dir);
    command
        .current_dir(&manifest_dir)
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .arg("--target")
        .arg(target)
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
    if let Some(target_override) = config.targets.overrides.get(target) {
        if !target_override.linker.is_empty() {
            command.env(cargo_target_linker_env(target), &target_override.linker);
        }
    }

    util::run_command(&mut command, args.verbose)?;

    let profile = if args.release { "release" } else { "debug" };
    let built_artifact = entry.artifact.as_deref().unwrap_or(&entry.binary);
    let built_binary = cargo_target_dir.join(target).join(profile).join(built_artifact);
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
    maybe_strip_staged_binary(&staged_binary, should_strip_packaged_binaries(args), args.verbose)?;
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
    maybe_strip_staged_binary(&staged_kernel, should_strip_packaged_binaries(args), args.verbose)?;

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
        maybe_strip_staged_binary(&staged_binary, should_strip_packaged_binaries(args), args.verbose)?;
        service["path"] = serde_json::Value::String(format!("../../bin/{name}"));
    }
    fs::write(runtime_root.join("services.json"), serde_json::to_string_pretty(&services)?)?;

    // build --hosted stages built-in bundles (manifest + app.elf) next to the
    // source tree, not under CARGO_TARGET_DIR; ship the app.elf the simulator execs.
    let built_apps_dir = keyos_root.join("target").join("hosted").join("keyos").join("apps");
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

fn maybe_strip_staged_binary(path: &Path, should_strip: bool, verbose: bool) -> Result<()> {
    if !should_strip || !path.is_file() || !is_strippable_binary(path)? {
        return Ok(());
    }

    let strip_program = find_strip_program().ok_or_else(|| {
        boxed_err("packaged SDK binaries require 'strip' or 'llvm-strip' to remove bundled debug info")
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
            maybe_strip_staged_binary(&path, should_strip, verbose)?;
        }
    }

    Ok(())
}

fn find_strip_program() -> Option<PathBuf> {
    ["strip", "llvm-strip"].into_iter().find_map(find_program_in_path)
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

fn nix_shell_active() -> bool {
    env::var_os("IN_NIX_SHELL").is_some()
        || env::var_os("NIX_BUILD_TOP").is_some()
        || env::var_os("FOUNDATION_SDK_ROOT").is_some()
}

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
    match entry.filter {
        CopyFilter::All => util::copy_dir_all(&source, &destination)?,
        CopyFilter::CargoPackage => stage_cargo_package_snapshot(&source, &destination)?,
        CopyFilter::SlintSdk => stage_slint_sdk_snapshot(&source, &destination)?,
    }
    Ok(())
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
    dest.starts_with("lib/keyos/")
        && !dest.starts_with("lib/keyos/keyos")
        && !dest.starts_with("lib/keyos/ui")
        && !dest.starts_with(KEYOS_HOSTED_RUNTIME_ROOT)
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

fn build_docs(root: &Path, stage_dir: &Path, target: &str, config: &Config, verbose: bool) -> Result<()> {
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

    let doc_target_dir = root.join("target").join("xtask-docs").join(target);
    util::ensure_clean_dir(&doc_target_dir)?;

    let mut generated_any_docs = false;
    let mut skipped = Vec::new();

    for crate_path in &config.docs.api_crates {
        let manifest_path = stage_dir.join(crate_path).join("Cargo.toml");
        if !manifest_path.exists() {
            skipped.push(crate_path.clone());
            continue;
        }

        let mut command = Command::new("cargo");
        command
            .arg("doc")
            .arg("--no-deps")
            .arg("--manifest-path")
            .arg(&manifest_path)
            .arg("--target-dir")
            .arg(&doc_target_dir);
        util::run_command(&mut command, verbose)?;
        generated_any_docs = true;
    }

    for crate_name in &config.docs.api_crates_workspace {
        let mut command = Command::new("cargo");
        command
            .arg("doc")
            .arg("--no-deps")
            .arg("-p")
            .arg(crate_name)
            .arg("--target-dir")
            .arg(&doc_target_dir);
        util::run_command(&mut command, verbose)?;
        generated_any_docs = true;
    }

    if generated_any_docs {
        let generated_docs = doc_target_dir.join("doc");
        if generated_docs.exists() {
            util::copy_dir_contents(&generated_docs, &api_dir)?;
        }
    } else {
        fs::write(
            api_dir.join("README.md"),
            "# API Docs\n\nAPI docs will be generated here once the referenced crates are available.\n",
        )?;
    }

    if !skipped.is_empty() {
        fs::write(
            api_dir.join("SKIPPED.md"),
            format!(
                "# Skipped API Crates\n\nThe following copied crates were not present during this build:\n\n{}\n",
                skipped
                    .iter()
                    .map(|item| format!("- `{item}`"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )?;
    }

    util::copy_file_if_exists(&root.join("AGENTS.md"), &docs_dir.join("AGENTS.md"))?;
    Ok(())
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
        Command::new("git").arg("-C").arg(path).arg("status").arg("--porcelain").arg("--untracked-files=no"),
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
    use std::path::{Path, PathBuf};

    use super::{
        cargo_target_linker_env, is_strippable_binary_header, keyos_slint_pin_from_manifest_and_lock,
        local_staged_workspace_dependency_override, nix_shell_active, parse_git_source_commit,
        parse_toolchain_channel, prune_nested_member_dirs, render_staged_keyos_workspace_manifest,
        render_staged_slint_workspace_manifest, rust_toolchain_channel, should_stage_simulator_for_target,
        should_strip_packaged_binaries, stage_cargo_package_snapshot, stage_shared_ui_artifact,
        stage_slint_sdk_snapshot, verify_common_stage, verify_target_stage, BuildArgs, SmokeCheckArgs,
        StageDirLock,
    };
    use crate::config::workspace_root;

    #[test]
    fn stage_dir_lock_blocks_concurrent_holders() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let output_dir = env::temp_dir().join(format!("foundation-stage-lock-{unique}"));
        let stage_root = output_dir.join(".stage");
        fs::create_dir_all(&output_dir).unwrap();

        let first = StageDirLock::acquire(&output_dir, &stage_root).expect("first lock should succeed");
        let second = StageDirLock::acquire(&output_dir, &stage_root);
        assert!(second.is_err(), "second lock acquisition should fail while first is held");
        let err = second.unwrap_err().to_string();
        assert!(err.contains("another xtask invocation"), "expected lock-contention message, got: {err}");

        // Releasing the first lock should let a new caller succeed.
        drop(first);
        let third = StageDirLock::acquire(&output_dir, &stage_root)
            .expect("lock should be reacquirable after release");
        drop(third);

        let _ = fs::remove_dir_all(&output_dir);
    }

    #[test]
    fn cargo_target_linker_env_matches_cargo_convention() {
        assert_eq!(
            cargo_target_linker_env("aarch64-unknown-linux-gnu"),
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER"
        );
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
        let stage_dir = temp_stage_dir("verify-stage");
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
        fs::remove_dir_all(stage_dir).unwrap();
    }

    #[test]
    fn verify_common_stage_requires_shared_layout() {
        let stage_dir = temp_stage_dir("verify-common-stage");
        fs::create_dir_all(stage_dir.join("docs").join("guide").join("src")).unwrap();
        fs::create_dir_all(stage_dir.join("docs").join("api")).unwrap();
        for tool_dir in [".agents", ".claude"] {
            for skill in ["foundation-cli", "foundation-localize", "foundation-new-page"] {
                fs::create_dir_all(stage_dir.join(tool_dir).join("skills").join(skill)).unwrap();
            }
        }
        fs::create_dir_all(stage_dir.join("lib").join("keyos")).unwrap();
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

        for path in [
            stage_dir.join("lib").join("keyos").join("Cargo.toml"),
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
                .join("default_theme.json"),
            stage_dir.join("ui").join("ui").join("theme.slint"),
            stage_dir.join("lib").join("keyos").join("utils").join("fiat-symbols").join("Cargo.toml"),
            stage_dir.join("resources").join("icons").join("loader.svg"),
            stage_dir.join(".agents").join("skills").join("foundation-cli").join("SKILL.md"),
            stage_dir.join(".agents").join("skills").join("foundation-localize").join("SKILL.md"),
            stage_dir.join(".agents").join("skills").join("foundation-new-page").join("SKILL.md"),
            stage_dir.join(".claude").join("skills").join("foundation-cli").join("SKILL.md"),
            stage_dir.join(".claude").join("skills").join("foundation-localize").join("SKILL.md"),
            stage_dir.join(".claude").join("skills").join("foundation-new-page").join("SKILL.md"),
            stage_dir.join("docs").join("guide").join("src").join("foundation-cli.md"),
            stage_dir.join("flake.nix"),
            stage_dir.join("setup.sh"),
        ] {
            fs::write(path, "").unwrap();
        }

        assert!(verify_common_stage(&stage_dir, false).is_ok());
        fs::remove_dir_all(stage_dir).unwrap();
    }

    #[test]
    fn shared_ui_is_generated_into_artifact_stage() {
        let root = workspace_root();
        let keyos_root = root.parent().expect("sdk workspace has KeyOS parent");
        let stage_dir = temp_stage_dir("shared-ui-artifact");

        stage_shared_ui_artifact(keyos_root, &stage_dir, false).unwrap();

        assert!(stage_dir.join("ui").join("ui").join("theme.slint").exists());
        assert!(stage_dir.join("lib").join("keyos").join("ui").join("ui").join("theme.slint").exists());
        assert!(stage_dir.join("resources").join("icons").join("loader.svg").exists());
        // Theme JSON is shipped by the foundation-themes crate copy (the
        // cargo_package snapshot, covered by
        // stage_cargo_package_snapshot_omits_target_directory), not by the
        // shared-UI staging here.

        fs::remove_dir_all(stage_dir).unwrap();
    }

    #[test]
    fn parse_toolchain_channel_reads_rust_toolchain_toml() {
        let stage_dir = temp_stage_dir("toolchain-channel");
        let toolchain = stage_dir.join("rust-toolchain.toml");
        fs::write(&toolchain, "[toolchain]\nchannel = \"nightly-2025-09-11\"\n").unwrap();

        assert_eq!(parse_toolchain_channel(&toolchain).as_deref(), Some("nightly-2025-09-11"));
        fs::remove_dir_all(stage_dir).unwrap();
    }

    #[test]
    fn rust_toolchain_channel_walks_parent_directories() {
        let root = temp_stage_dir("toolchain-search");
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("rust-toolchain.toml"), "[toolchain]\nchannel = \"nightly-2025-09-11\"\n")
            .unwrap();

        assert_eq!(rust_toolchain_channel(&nested).as_deref(), Some("nightly-2025-09-11"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nix_shell_active_detects_foundation_sdk_root() {
        let previous = env::var_os("FOUNDATION_SDK_ROOT");
        env::set_var("FOUNDATION_SDK_ROOT", "/tmp/foundation-sdk");

        assert!(nix_shell_active());

        if let Some(value) = previous {
            env::set_var("FOUNDATION_SDK_ROOT", value);
        } else {
            env::remove_var("FOUNDATION_SDK_ROOT");
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
            &["server".to_string(), "server/macro".to_string()],
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
        assert_eq!(members, vec!["server", "server/macro"]);

        let dependencies = workspace.get("dependencies").and_then(toml::Value::as_table).unwrap();
        for dependency in ["defer", "log", "log-server", "rkyv", "whence", "xous-names"] {
            assert!(dependencies.contains_key(dependency));
        }
    }

    #[test]
    fn staged_workspace_uses_local_slint_paths_when_packaged() {
        let root = temp_stage_dir("slint-override");
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

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_slint_workspace_manifest_filters_missing_members_and_paths() {
        let root = temp_stage_dir("slint-manifest");
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

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn slint_snapshot_excludes_local_build_artifacts() {
        let source_root = temp_stage_dir("slint-source");
        let destination_root = temp_stage_dir("slint-destination");

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

        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn stage_cargo_package_snapshot_omits_target_directory() {
        let source_root = temp_stage_dir("cargo-package-source");
        let destination_root = temp_stage_dir("cargo-package-dest");

        fs::create_dir_all(source_root.join("src")).unwrap();
        fs::create_dir_all(source_root.join("themes")).unwrap();
        fs::create_dir_all(source_root.join("target").join("debug")).unwrap();
        fs::write(source_root.join("Cargo.toml"), "[package]\nname = \"foundation-themes\"\n").unwrap();
        fs::write(source_root.join("src").join("lib.rs"), "pub mod build;\n").unwrap();
        fs::write(source_root.join("themes").join("default_theme.json"), "{}").unwrap();
        fs::write(source_root.join("target").join("debug").join("ignored"), "ignored").unwrap();

        stage_cargo_package_snapshot(&source_root, &destination_root).unwrap();

        assert!(destination_root.join("Cargo.toml").exists());
        assert!(destination_root.join("src").join("lib.rs").exists());
        assert!(destination_root.join("themes").join("default_theme.json").exists());
        assert!(!destination_root.join("target").exists());

        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    fn temp_stage_dir(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("foundation-xtask-{label}-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
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
