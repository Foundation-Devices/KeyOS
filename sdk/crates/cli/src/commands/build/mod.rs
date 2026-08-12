// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Build KeyOS application for hardware

use std::collections::BTreeMap;
use std::fs;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use app_archive::ELF_FILE;
use clap::Args;
use foundation_core::{
    app_manifest_from_config, configured_signing_identities, is_valid_identity_name, signing_identity_paths,
    AppConfig, ProjectContext, SdkRoot, SigningIdentityPaths, FILE_HASH_BYTE_LEN,
};
use foundation_ui::Prompts;

#[derive(Args)]
pub struct BuildArgs {
    /// Build in release mode with optimizations
    #[arg(short, long)]
    pub release: bool,
}

use crate::assets::stage_hardware_assets;
use crate::cargo_support::{emit_cargo_messages, emit_stderr_if_present, ensure_development_environment};
use crate::signing_permissions::{
    ensure_signing_directory, repair_private_key_permissions, SigningDirectoryStatus,
};
use crate::slint_codegen::{prepare_project_for_build, project_sdk_ui_root, UI_LIBRARY_PATH_ENV};

/// Target triple for KeyOS hardware builds
const TARGET_TRIPLE: &str = "armv7a-unknown-xous-elf";

/// RUSTFLAGS for PIC builds
///
/// `-Zunstable-options` is required since 1.96.0 nightlies to load custom (JSON) target
/// specifications such as armv7a-unknown-xous-elf.
const RUSTFLAGS_PIC: &str = "--cfg keyos -C relocation-model=pic -C link-arg=-pie -Zunstable-options";

/// What a build left on disk, for the commands that run one before doing their own work.
pub struct BuiltBundle {
    pub bundle_dir: PathBuf,
    /// Bundle-relative names of the files the manifest's `fileHashes` covers.
    pub hashed_files: Vec<String>,
}

/// Execute the build command
pub fn execute(args: &BuildArgs) -> Result<BuiltBundle> {
    let release = args.release;

    println!("Building KeyOS application...");

    // Check nix environment
    ensure_development_environment("foundation build")?;
    let sdk = SdkRoot::discover()
        .context("Could not locate the Foundation SDK root from the active development shell.")?;

    // Find and read app-config.toml
    println!("Reading app-config.toml...");
    let project = ProjectContext::discover()?;
    let project_root = project.root.as_path();
    let config = &project.config;

    // Build the bundle from a clean dir, or else a stray root file (a stale
    // artifact, a .DS_Store) gets hashed into the signed manifest.
    let output_dir = project_root.join("target").join("keyos").join(&config.app_name);
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir)
            .with_context(|| format!("Failed to clean bundle directory {}", output_dir.display()))?;
    }
    fs::create_dir_all(&output_dir)?;

    // Ensure shared @ui sources and generated router/translation files exist before cargo runs.
    prepare_project_for_build(project_root, &sdk)?;

    // Ensure the app's theme Rust is generated and current; theme.rs pulls it
    // in via foundation_themes::include_theme!, keyed on FOUNDATION_THEMES_RUST_DIR.
    let themes_rust_dir = crate::commands::themes::ensure_project_theme(&sdk, config, project_root)?;

    // Run cargo build
    println!("Running cargo build...");
    println!("  Target: armv7a-unknown-xous-elf");
    run_cargo_build(project_root, sdk.root(), config, release, &themes_rust_dir)?;

    // Find the built binary
    let profile = if release { "release" } else { "debug" };
    let binary_path = project_root.join("target").join(TARGET_TRIPLE).join(profile).join(&config.app_name);

    if !binary_path.exists() {
        anyhow::bail!("Cargo build failed: {}", binary_path.display());
    }

    // Strip binary
    println!("Stripping binary...");
    let stripped_path = output_dir.join("app.elf");
    strip_binary(&binary_path, &stripped_path)?;

    println!("Preparing app assets...");
    stage_hardware_assets(config, project_root, &output_dir)?;

    println!("Generating manifest.json...");
    let file_hashes = bundle_file_hashes(&output_dir)?;
    let hashed_files = file_hashes.keys().cloned().collect();
    let app_hash = file_hashes.get(ELF_FILE).cloned().unwrap_or_default();
    let manifest_path = output_dir.join("manifest.json");
    generate_manifest(config, project_root, &sdk, &manifest_path, file_hashes)?;

    // Sign app.elf and the manifest. fileHashes was taken from the unsigned elf, so signing the
    // elf doesn't invalidate it and the two signatures are independent.
    println!("Signing app.elf and manifest...");
    let cosign2_config_path = get_cosign2_config(config, project_root)?;
    sign_with_cosign2(&stripped_path, &cosign2_config_path, &config.version.to_string())?;
    ensure_cosign2_header(&stripped_path)?;
    sign_with_cosign2(&manifest_path, &cosign2_config_path, &config.version.to_string())?;

    // Success message
    println!();
    println!("Build complete!");
    println!("Output: {}", output_dir.display());
    println!("  app.elf (signed)");
    println!("  manifest.json (signed)");
    println!("  icon.bin");
    if config.dark_icon(project_root).is_some() {
        println!("  icon-dark.bin");
    }
    println!("  resources/");
    println!("Version: {}", config.version);
    println!("App hash: {}", hex::encode(app_hash));
    println!("Compare it with App Hash under Settings > Apps > {} on the device.", config.app_name);

    Ok(BuiltBundle { bundle_dir: output_dir, hashed_files })
}

/// Get the effective cosign2 config path for this app.
fn get_cosign2_config(config: &AppConfig, project_root: &Path) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;

    let (config_path, managed_identity) = match &config.cosign2_config {
        Some(path) => (resolve_explicit_cosign2_config(path, project_root, &home), None),
        None => {
            let identity = resolve_signing_identity(config)?;
            (identity.cosign2_config.clone(), Some(identity))
        }
    };

    if !config_path.exists() {
        anyhow::bail!(
            "cosign2 config not found: {}. Run 'foundation cert gen' first.",
            config_path.display()
        );
    }

    if let Some(identity) = managed_identity {
        harden_managed_signing_identity(&identity)?;
    }

    Ok(config_path)
}

fn resolve_explicit_cosign2_config(path: &str, project_root: &Path, home: &Path) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        return home.join(stripped);
    }

    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

fn resolve_signing_identity(config: &AppConfig) -> Result<SigningIdentityPaths> {
    if let Some(identity_name) =
        config.signing_identity.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        if !is_valid_identity_name(identity_name) {
            anyhow::bail!(
                "Invalid signing identity name '{}'. It cannot be empty or contain path separators.",
                identity_name
            );
        }
        return Ok(signing_identity_paths(identity_name)?);
    }

    let identities = configured_signing_identities()?;

    if let Some(publisher_name) = config.publisher.name_value() {
        if let Some(identity) = identities.iter().find(|identity| identity.identity_name == publisher_name) {
            return Ok(identity.clone());
        }
    }

    match identities.as_slice() {
        [] => anyhow::bail!(
            "No signing identity is configured. Run 'foundation cert gen' first, set 'signing-identity' in app-config.toml, or set 'cosign2-config' explicitly."
        ),
        [identity] => Ok(identity.clone()),
        _ => prompt_for_signing_identity(&identities),
    }
}

fn prompt_for_signing_identity(identities: &[SigningIdentityPaths]) -> Result<SigningIdentityPaths> {
    if !std::io::stderr().is_terminal() {
        anyhow::bail!(
            "Multiple signing identities are configured ({}), but this build is not running in an interactive terminal. Set 'signing-identity' or 'cosign2-config' in app-config.toml.",
            identities
                .iter()
                .map(|identity| identity.identity_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let options: Vec<&str> = identities.iter().map(|identity| identity.identity_name.as_str()).collect();
    let selection = Prompts::new()
        .select("Select a publisher signing identity", &options)
        .context("Failed to choose a signing identity")?;
    Ok(identities[selection].clone())
}

fn harden_managed_signing_identity(identity: &SigningIdentityPaths) -> Result<()> {
    for directory in [identity.root.parent(), Some(identity.root.as_path())].into_iter().flatten() {
        match ensure_signing_directory(directory)
            .with_context(|| format!("Failed to secure signing directory: {}", directory.display()))?
        {
            SigningDirectoryStatus::Repaired => {
                eprintln!(
                    "Warning: removed group/world permissions from signing directory: {}",
                    directory.display()
                );
            }
            SigningDirectoryStatus::Created | SigningDirectoryStatus::Unchanged => {}
        }
    }

    if repair_private_key_permissions(&identity.private_key).with_context(|| {
        format!("Failed to secure private key permissions: {}", identity.private_key.display())
    })? {
        eprintln!(
            "Warning: removed group/world permissions from private key: {}",
            identity.private_key.display()
        );
    }

    Ok(())
}

/// Run cargo build with appropriate flags
fn run_cargo_build(
    project_root: &Path,
    sdk_root: &Path,
    config: &AppConfig,
    release: bool,
    themes_rust_dir: &Path,
) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_root);
    cmd.arg("build");
    cmd.arg("--target").arg(TARGET_TRIPLE);
    cmd.arg("--package").arg(&config.app_name);
    cmd.arg("--message-format").arg("json-render-diagnostics");

    if release {
        cmd.arg("--release");
    }

    // Set RUSTFLAGS for PIC
    cmd.env("RUSTFLAGS", RUSTFLAGS_PIC);
    // Resolve foundation_themes::include_theme! against the generated theme dir.
    cmd.env("FOUNDATION_THEMES_RUST_DIR", themes_rust_dir);
    cmd.env(UI_LIBRARY_PATH_ENV, project_sdk_ui_root(project_root));
    // `@theme` namespace → per-app generated component themes.
    cmd.env("FOUNDATION_THEMES_SLINT_DIR", crate::commands::themes::project_theme_slint_dir(project_root));

    let output = cmd.output().context("cargo not found")?;
    emit_cargo_messages(project_root, sdk_root, &output.stdout);
    emit_stderr_if_present(project_root, sdk_root, &output.stderr);

    if !output.status.success() {
        anyhow::bail!("Cargo build failed");
    }

    Ok(())
}

/// Strip the binary using arm-none-eabi-strip
fn strip_binary(input: &Path, output: &Path) -> Result<()> {
    // Check if strip is available
    if !Command::new("arm-none-eabi-strip")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        anyhow::bail!("arm-none-eabi-strip not found. Is the nix environment active?");
    }

    let status = Command::new("arm-none-eabi-strip")
        .args(["--strip-unneeded", "-o"])
        .arg(output)
        .arg(input)
        .status()
        .context("Failed to strip binary")?;

    if !status.success() {
        anyhow::bail!("Failed to strip binary");
    }

    Ok(())
}

/// Generate manifest.json from app-config.toml, carrying the staged bundle's file hashes.
fn generate_manifest(
    config: &AppConfig,
    project_root: &Path,
    sdk: &SdkRoot,
    output: &Path,
    file_hashes: BTreeMap<String, [u8; FILE_HASH_BYTE_LEN]>,
) -> Result<()> {
    let permissions = config.resolved_permissions(project_root, Some(&sdk.keyos_root().join("api")))?;
    let mut manifest = app_manifest_from_config(config, permissions);
    manifest.file_hashes = file_hashes;
    let json = serde_json::to_string_pretty(&manifest)?;
    fs::write(output, json)?;

    Ok(())
}

/// Sha256 of every staged bundle file except `manifest.json`, keyed by bundle-relative path
/// with forward slashes. The manifest is the signed container, so it never lists its own hash.
fn bundle_file_hashes(bundle_dir: &Path) -> Result<BTreeMap<String, [u8; FILE_HASH_BYTE_LEN]>> {
    use sha2::{Digest, Sha256};

    let mut hashes = BTreeMap::new();
    let mut stack = vec![bundle_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(bundle_dir)?.to_string_lossy().replace('\\', "/");
            if rel == "manifest.json" {
                continue;
            }
            hashes.insert(rel, Sha256::digest(fs::read(&path)?).into());
        }
    }
    Ok(hashes)
}

/// Sign a bundle file in place using cosign2 with the developer (slot 2) scheme.
fn sign_with_cosign2(input_path: &Path, cosign2_config: &Path, version: &str) -> Result<()> {
    let sdk = SdkRoot::discover().ok();

    let status = if let Some(cosign2) =
        sdk.as_ref().and_then(|sdk| sdk.tool_path(&["cosign2"])).or_else(|| find_in_path("cosign2"))
    {
        Command::new(cosign2)
            .arg("sign")
            .arg("--developer")
            .arg("--in-place")
            .arg("-i")
            .arg(input_path)
            .arg("--binary-version")
            .arg(version)
            .arg("-c")
            .arg(cosign2_config)
            .status()
            .context("Failed to sign bundle file")?
    } else if let Some(manifest) = sdk
        .as_ref()
        .map(|sdk| sdk.keyos_root().join("imports").join("cosign2").join("cosign2-bin").join("Cargo.toml"))
        .filter(|manifest| manifest.exists())
    {
        Command::new("cargo")
            .arg("run")
            .arg("--manifest-path")
            .arg(manifest)
            .arg("--bin")
            .arg("cosign2")
            .arg("--")
            .arg("sign")
            .arg("--developer")
            .arg("--in-place")
            .arg("-i")
            .arg(input_path)
            .arg("--binary-version")
            .arg(version)
            .arg("-c")
            .arg(cosign2_config)
            .status()
            .context("Failed to sign bundle file")?
    } else {
        anyhow::bail!("cosign2 not found. Is the nix environment active?");
    };

    if !status.success() {
        anyhow::bail!("Failed to sign {}", input_path.display());
    }

    Ok(())
}

/// Confirm app.elf carries a cosign2 header after signing, so an unsigned binary never ships as if
/// it were signed.
fn ensure_cosign2_header(elf_path: &Path) -> Result<()> {
    let mut file = fs::File::open(elf_path)
        .with_context(|| format!("Failed to inspect signed application {}", elf_path.display()))?;
    let mut header = vec![0; cosign2::Header::DEFAULT_SIZE];
    file.read_exact(&mut header).with_context(|| {
        format!("Signed application {} is too small to contain a cosign2 header", elf_path.display())
    })?;

    match cosign2::Header::parse_unverified(&header, cosign2::Header::DEFAULT_SIZE, false) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => anyhow::bail!("Signed application {} is missing a cosign2 header", elf_path.display()),
        Err(e) => {
            anyhow::bail!("Signed application {} has an invalid cosign2 header: {e:?}", elf_path.display())
        }
    }
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(command);
        candidate.is_file().then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use crate::cargo_support::{filter_cargo_stderr, rendered_compiler_message};
    use crate::test_support::make_temp_dir;

    #[cfg(unix)]
    #[test]
    fn managed_signing_identity_repairs_permissions_before_use() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let root_dir = make_temp_dir("build-signing-permissions");
        let root = root_dir.path();
        let signing_root = root.join("signing");
        let identity_root = signing_root.join("demo");
        fs::create_dir_all(&identity_root).unwrap();
        fs::set_permissions(&signing_root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&identity_root, fs::Permissions::from_mode(0o775)).unwrap();

        let identity = foundation_core::SigningIdentityPaths::new("demo", identity_root.clone());
        fs::write(&identity.private_key, b"private key").unwrap();
        fs::set_permissions(&identity.private_key, fs::Permissions::from_mode(0o644)).unwrap();

        super::harden_managed_signing_identity(&identity).unwrap();

        assert_eq!(fs::metadata(&signing_root).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&identity_root).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&identity.private_key).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn suppresses_sdk_dependency_warnings() {
        let line = json!({
            "reason": "compiler-message",
            "message": {
                "level": "warning",
                "rendered": "warning: sdk dependency warning\n",
                "spans": [{
                    "file_name": "/tmp/sdk/lib/slint/internal/core/lib.rs"
                }]
            }
        })
        .to_string();

        let rendered = rendered_compiler_message(&line, Path::new("/tmp/project"), Path::new("/tmp/sdk"));

        assert!(rendered.is_none());
    }

    #[test]
    fn keeps_project_warnings_visible() {
        let line = json!({
            "reason": "compiler-message",
            "message": {
                "level": "warning",
                "rendered": "warning: app warning\n",
                "spans": [{
                    "file_name": "/tmp/project/src/main.rs"
                }]
            }
        })
        .to_string();

        let rendered = rendered_compiler_message(&line, Path::new("/tmp/project"), Path::new("/tmp/sdk"));

        assert_eq!(rendered.as_deref(), Some("warning: app warning\n"));
    }

    #[test]
    fn filters_sdk_warning_blocks_from_stderr() {
        let stderr = r#"warning: dependency warning
 --> /tmp/sdk/lib/slint/internal/core/lib.rs:1:1
  |
1 | foo
  | ^^^

warning: `i-slint-core` (lib) generated 1 warning

    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.31s
"#;

        let filtered = filter_cargo_stderr(stderr, Path::new("/tmp/project"), Path::new("/tmp/sdk"));

        assert!(!filtered.contains("dependency warning"));
        assert!(!filtered.contains("generated 1 warning"));
        assert!(filtered.contains("Finished `dev` profile"));
    }

    #[test]
    fn keeps_project_warning_blocks_in_stderr() {
        let stderr = r#"warning: app warning
 --> /tmp/project/src/main.rs:1:1
  |
1 | foo
  | ^^^
"#;

        let filtered = filter_cargo_stderr(stderr, Path::new("/tmp/project"), Path::new("/tmp/sdk"));

        assert!(filtered.contains("app warning"));
    }

    #[test]
    fn cosign2_header_guard_rejects_unsigned_artifact() {
        let artifact_dir = make_temp_dir("unsigned-app");
        let artifact = artifact_dir.path().join("app.elf");
        std::fs::write(&artifact, b"unsigned app").unwrap();

        let error = super::ensure_cosign2_header(&artifact).unwrap_err().to_string();

        assert!(error.contains("too small to contain a cosign2 header"));
    }
}
