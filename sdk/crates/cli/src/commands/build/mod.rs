// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Build KeyOS application for hardware

use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use foundation_core::{
    configured_signing_identities, resolve_identity_cosign2_config, AppConfig, AppManifest, ProjectContext,
    SdkRoot,
};
use foundation_ui::Prompts;

use crate::assets::stage_hardware_assets;
use crate::cargo_support::{emit_cargo_messages, emit_stderr_if_present};
use crate::slint_codegen::{prepare_project_for_build, project_sdk_ui_root, UI_LIBRARY_PATH_ENV};

/// Target triple for KeyOS hardware builds
const TARGET_TRIPLE: &str = "armv7a-unknown-xous-elf";

/// RUSTFLAGS for PIC builds
const RUSTFLAGS_PIC: &str = "--cfg keyos -C relocation-model=pic -C link-arg=-pie";

/// Execute the build command
pub fn execute(release: bool) -> Result<()> {
    println!("Building KeyOS application...");

    // Check nix environment
    check_nix_environment()?;
    let sdk = SdkRoot::discover()
        .context("Could not locate the Foundation SDK root from the active development shell.")?;

    // Find and read app-config.toml
    println!("Reading app-config.toml...");
    let project = ProjectContext::discover()
        .context("app-config.toml not found. Are you in a Foundation app directory?")?;
    let project_root = project.root.as_path();
    let config = &project.config;

    // Create output directory
    let output_dir = project_root.join("target").join("keyos").join(&config.app_name);
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

    // Generate manifest.json
    println!("Generating manifest.json...");
    let manifest_path = output_dir.join("manifest.json");
    generate_manifest(config, project_root, &manifest_path)?;

    println!("Preparing app assets...");
    stage_hardware_assets(config, project_root, &output_dir)?;

    // Sign the application
    println!("Signing application...");
    let cosign2_config_path = get_cosign2_config(config, project_root)?;
    sign_application(&stripped_path, &cosign2_config_path, &config.version.to_string())?;
    ensure_cosign2_header(&stripped_path)?;

    // Success message
    println!();
    println!("Build complete!");
    println!("Output: {}", output_dir.display());
    println!("  app.elf (signed)");
    println!("  manifest.json");
    println!("  icon.bin");
    println!("  resources/");
    println!("Version: {}", config.version);

    Ok(())
}

/// Check if we're in a nix environment
fn check_nix_environment() -> Result<()> {
    println!("Checking Nix environment...");

    // Check for nix environment indicators
    let in_nix = std::env::var("IN_NIX_SHELL").is_ok()
        || std::env::var("NIX_BUILD_TOP").is_ok()
        || std::env::var("FOUNDATION_SDK_ROOT").is_ok();

    if !in_nix {
        eprintln!("Nix development environment is not active.");
        print!("Setup nix environment now? [Y/n]: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();

        if input.is_empty() || input == "y" || input == "yes" {
            // Run foundation develop
            let status = Command::new("foundation").arg("develop").status();

            match status {
                Ok(s) if s.success() => {
                    // User exited the nix shell, remind them to re-run
                    println!("Please run 'foundation build' again inside the nix shell.");
                    std::process::exit(0);
                }
                _ => {
                    anyhow::bail!("Failed to start nix environment");
                }
            }
        } else {
            anyhow::bail!("Build requires nix environment. Run 'foundation develop' first.");
        }
    }

    Ok(())
}

/// Get the effective cosign2 config path for this app.
fn get_cosign2_config(config: &AppConfig, project_root: &Path) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;

    let config_path = match &config.cosign2_config {
        Some(path) => resolve_explicit_cosign2_config(path, project_root, &home),
        None => resolve_signing_identity_config(config)?,
    };

    if !config_path.exists() {
        anyhow::bail!(
            "cosign2 config not found: {}. Run 'foundation cert gen' first.",
            config_path.display()
        );
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

fn resolve_signing_identity_config(config: &AppConfig) -> Result<PathBuf> {
    if let Some(identity_name) =
        config.signing_identity.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        return Ok(resolve_identity_cosign2_config(identity_name)?);
    }

    let identities = configured_signing_identities()?;

    if let Some(publisher_name) = config.publisher.name_value() {
        if let Some(identity) = identities.iter().find(|identity| identity.identity_name == publisher_name) {
            return Ok(identity.cosign2_config.clone());
        }
    }

    match identities.as_slice() {
        [] => anyhow::bail!(
            "No signing identity is configured. Run 'foundation cert gen' first, set 'signing-identity' in app-config.toml, or set 'cosign2-config' explicitly."
        ),
        [identity] => Ok(identity.cosign2_config.clone()),
        _ => prompt_for_signing_identity(&identities),
    }
}

fn prompt_for_signing_identity(identities: &[foundation_core::SigningIdentityPaths]) -> Result<PathBuf> {
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
    Ok(identities[selection].cosign2_config.clone())
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
    // `@theme` namespace → per-app generated component themes (button_theme.slint).
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

/// Generate manifest.json from app-config.toml
fn generate_manifest(config: &AppConfig, project_root: &Path, output: &Path) -> Result<()> {
    let manifest = AppManifest::from_config(config, config.resolved_permissions(project_root)?);
    let json = serde_json::to_string_pretty(&manifest)?;
    fs::write(output, json)?;

    Ok(())
}

/// Sign the application using cosign2
fn sign_application(elf_path: &Path, cosign2_config: &Path, version: &str) -> Result<()> {
    let sdk = SdkRoot::discover().ok();

    let status = if let Some(cosign2) =
        sdk.as_ref().and_then(|sdk| sdk.tool_path(&["cosign2"])).or_else(|| find_in_path("cosign2"))
    {
        Command::new(cosign2)
            .arg("sign")
            .arg("--developer")
            .arg("--in-place")
            .arg("-i")
            .arg(elf_path)
            .arg("--binary-version")
            .arg(version)
            .arg("-c")
            .arg(cosign2_config)
            .status()
            .context("Failed to sign application")?
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
            .arg(elf_path)
            .arg("--binary-version")
            .arg(version)
            .arg("-c")
            .arg(cosign2_config)
            .status()
            .context("Failed to sign application")?
    } else {
        anyhow::bail!("cosign2 not found. Is the nix environment active?");
    };

    if !status.success() {
        anyhow::bail!("Failed to sign application");
    }

    Ok(())
}

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
        let artifact = std::env::temp_dir().join(format!(
            "foundation-unsigned-app-{}.elf",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::write(&artifact, b"unsigned app").unwrap();

        let error = super::ensure_cosign2_header(&artifact).unwrap_err().to_string();

        assert!(error.contains("too small to contain a cosign2 header"));
        let _ = std::fs::remove_file(artifact);
    }
}
