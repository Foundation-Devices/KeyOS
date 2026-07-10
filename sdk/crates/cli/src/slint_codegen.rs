// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use foundation_core::{app_manifest_from_config, AppConfig, SdkRoot, APP_CONFIG_FILE};

use crate::sdk_mapping::ensure_project_sdk_mapping;

const COMPILE_MANIFEST_FILE: &str = "manifest.toml";
pub const UI_LIBRARY_PATH_ENV: &str = "FOUNDATION_UI_LIBRARY_PATH";

pub fn prepare_project_for_build(project_root: &Path, sdk: &SdkRoot) -> Result<()> {
    prepare_project_for_build_with_theme_dir(project_root, sdk, None)
}

fn prepare_project_for_build_with_theme_dir(
    project_root: &Path,
    sdk: &SdkRoot,
    themes_dir: Option<&Path>,
) -> Result<()> {
    ensure_project_sdk_mapping(project_root, sdk)?;
    ensure_compile_manifest(project_root)?;
    ensure_project_ui_mapping(project_root, sdk)?;
    maybe_generate_slint_artifacts(project_root, Some(sdk), CodegenMode::MissingOnly, themes_dir)
}

pub fn prepare_slint_file_for_view(slint_file: &Path, sdk: Option<&SdkRoot>) -> Result<()> {
    let cwd = std::env::current_dir().ok();
    prepare_slint_file_for_view_from(slint_file, sdk, cwd.as_deref())
}

fn prepare_slint_file_for_view_from(
    slint_file: &Path,
    sdk: Option<&SdkRoot>,
    cwd: Option<&Path>,
) -> Result<()> {
    let Some(project_root) = find_project_root_from(slint_file, cwd) else {
        return Ok(());
    };

    prepare_project_for_view(&project_root, sdk)
}

fn prepare_project_for_view(project_root: &Path, sdk: Option<&SdkRoot>) -> Result<()> {
    ensure_compile_manifest(&project_root)?;

    if let Some(sdk) = sdk {
        ensure_project_sdk_mapping(&project_root, sdk)?;
        ensure_project_ui_mapping(&project_root, sdk)?;
    }

    // Viewer workflows do not have a later cargo build step to refresh routed or localized
    // `ui/gen/*` files, so always try the app's existing build.rs codegen first.
    maybe_generate_slint_artifacts(&project_root, sdk, CodegenMode::AlwaysCheck, None)
}

fn ensure_compile_manifest(project_root: &Path) -> Result<()> {
    let config_path = project_root.join(APP_CONFIG_FILE);
    if !config_path.exists() {
        return Ok(());
    }

    let config =
        AppConfig::load(&config_path).with_context(|| format!("Failed to read {}", config_path.display()))?;
    let permissions = config
        .resolved_permissions(project_root)
        .with_context(|| format!("Failed to resolve permissions for {}", project_root.display()))?;
    let manifest = app_manifest_from_config(&config, permissions);
    let rendered =
        toml::to_string_pretty(&manifest).context("Failed to serialize compatibility manifest.toml")?;
    let manifest_path = project_root.join(COMPILE_MANIFEST_FILE);

    if fs::read_to_string(&manifest_path).ok().as_deref() == Some(rendered.as_str()) {
        return Ok(());
    }

    fs::write(&manifest_path, rendered)
        .with_context(|| format!("Failed to write {}", manifest_path.display()))?;
    Ok(())
}

fn maybe_generate_slint_artifacts(
    project_root: &Path,
    sdk: Option<&SdkRoot>,
    mode: CodegenMode,
    themes_dir: Option<&Path>,
) -> Result<()> {
    let build_rs = project_root.join("build.rs");
    if !build_rs.exists() {
        return Ok(());
    }

    let options = parse_build_options(&build_rs)?;
    let module_path = project_root.join(&options.module_path);
    if !module_path.exists() {
        anyhow::bail!(
            "Slint module configured in {} was not found at {}",
            build_rs.display(),
            module_path.display()
        );
    }

    let had_generated_files = generated_slint_files_exist(project_root, &options);
    if matches!(mode, CodegenMode::MissingOnly) && had_generated_files {
        return Ok(());
    }

    let package_name = read_package_name(&project_root.join("Cargo.toml"))?;
    let themes_rust_dir = theme_rust_dir_for_codegen(project_root, sdk, themes_dir)?;

    let local = run_codegen_check(project_root, &package_name, themes_rust_dir.as_deref())
        .with_context(|| format!("Failed to run cargo check for {}", project_root.display()))?;
    if local.status.success() || generated_slint_files_exist(project_root, &options) {
        return Ok(());
    }
    let local_error = command_output_summary("cargo check", &local);

    if let Some(sdk) = sdk {
        let fallback =
            run_nix_codegen_check(project_root, sdk.root(), &package_name, themes_rust_dir.as_deref())
                .with_context(|| {
                    format!("Failed to run nix develop cargo check for {}", project_root.display())
                })?;
        if fallback.status.success() || generated_slint_files_exist(project_root, &options) {
            return Ok(());
        }
        let fallback_error = command_output_summary("nix develop cargo check", &fallback);
        let detail = if fallback_error.is_empty() { local_error } else { fallback_error };
        if matches!(mode, CodegenMode::AlwaysCheck) && had_generated_files {
            return Ok(());
        }
        anyhow::bail!(
            "Failed to generate Slint artifacts for {}. Run `foundation develop` or `foundation build` for more detail: {}",
            project_root.display(),
            detail
        );
    }

    if matches!(mode, CodegenMode::AlwaysCheck) && had_generated_files {
        return Ok(());
    }

    anyhow::bail!(
        "Failed to generate Slint artifacts for {}. Run `foundation develop` or `foundation build` for more detail: {}",
        project_root.display(),
        local_error
    );
}

pub fn ensure_project_ui_mapping(project_root: &Path, sdk: &SdkRoot) -> Result<()> {
    let sdk_ui_root = sdk.ui_library_path();
    if !sdk_ui_root.is_dir() {
        anyhow::bail!("SDK UI library not found at {}", sdk_ui_root.display());
    }
    let sdk_ui_shared_resources_root = sdk.ui_shared_resources_path();
    if !sdk_ui_shared_resources_root.is_dir() {
        anyhow::bail!("SDK shared UI resources not found at {}", sdk_ui_shared_resources_root.display());
    }

    let private_ui_root = project_sdk_ui_root(project_root);
    let private_resources_root = project_sdk_resources_root(project_root);
    refresh_dir_from(&sdk_ui_root, &private_ui_root)?;
    refresh_dir_from(&sdk_ui_shared_resources_root, &private_resources_root)?;

    let app_resources_root = project_root.join("resources");
    if app_resources_root.is_dir() {
        copy_dir_all(&app_resources_root, &private_resources_root)?;
    }

    ensure_project_shared_tree_mapping(
        &project_root.join("ui"),
        &project_root.join("ui").join("ui"),
        &sdk_ui_root,
    )?;
    remove_legacy_shared_resource_symlinks(project_root, &sdk_ui_shared_resources_root)?;

    Ok(())
}

pub fn project_sdk_ui_root(project_root: &Path) -> PathBuf {
    project_root.join("target").join("foundation").join("ui").join("ui")
}

fn project_sdk_resources_root(project_root: &Path) -> PathBuf {
    project_root.join("target").join("foundation").join("resources")
}

fn remove_legacy_shared_resource_symlinks(project_root: &Path, sdk_resources_root: &Path) -> Result<()> {
    for dir in ["fonts", "icons", "images"] {
        let path = project_root.join("resources").join(dir);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let existing = fs::read_link(&path)?;
        let resolved = if existing.is_absolute() {
            existing
        } else {
            path.parent().unwrap_or_else(|| Path::new(".")).join(existing)
        };
        if resolved == sdk_resources_root.join(dir) {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn refresh_dir_from(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        fs::remove_dir_all(destination)
            .with_context(|| format!("Failed to clean generated directory {}", destination.display()))?;
    }
    copy_dir_all(source, destination)
}

fn parse_build_options(build_rs: &Path) -> Result<PreviewBuildOptions> {
    let build_rs_content =
        fs::read_to_string(build_rs).with_context(|| format!("Failed to read {}", build_rs.display()))?;
    let compact: String = build_rs_content.chars().filter(|ch| !ch.is_whitespace()).collect();

    Ok(PreviewBuildOptions {
        module_path: extract_string_value(&compact, "module_path:\"")
            .unwrap_or_else(|| "ui/app.slint".to_string()),
        include_router: extract_bool_value(&compact, "include_router:").unwrap_or(false),
        include_translations: extract_bool_value(&compact, "include_translations:").unwrap_or(false),
    })
}

fn read_package_name(cargo_toml: &Path) -> Result<String> {
    let content =
        fs::read_to_string(cargo_toml).with_context(|| format!("Failed to read {}", cargo_toml.display()))?;
    let document: toml::Value =
        toml::from_str(&content).with_context(|| format!("Failed to parse {}", cargo_toml.display()))?;
    document
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Missing package.name in {}", cargo_toml.display()))
}

fn theme_rust_dir_for_codegen(
    project_root: &Path,
    sdk: Option<&SdkRoot>,
    themes_dir: Option<&Path>,
) -> Result<Option<PathBuf>> {
    let Some(sdk) = sdk else {
        return Ok(None);
    };
    let config_path = project_root.join(APP_CONFIG_FILE);
    if !config_path.exists() {
        return Ok(None);
    }
    let config =
        AppConfig::load(&config_path).with_context(|| format!("Failed to read {}", config_path.display()))?;
    let rust_dir = match themes_dir {
        Some(themes_dir) => {
            crate::commands::themes::ensure_project_theme_in(sdk, &config, project_root, themes_dir)?
        }
        None => crate::commands::themes::ensure_project_theme(sdk, &config, project_root)?,
    };
    Ok(Some(rust_dir))
}

fn run_codegen_check(
    project_root: &Path,
    package_name: &str,
    themes_rust_dir: Option<&Path>,
) -> Result<std::process::Output> {
    let mut command = Command::new("cargo");
    command.current_dir(project_root).arg("check").arg("--quiet").arg("--package").arg(package_name);
    command.env(UI_LIBRARY_PATH_ENV, project_sdk_ui_root(project_root));
    command
        .env("FOUNDATION_THEMES_SLINT_DIR", crate::commands::themes::project_theme_slint_dir(project_root));
    if let Some(themes_rust_dir) = themes_rust_dir {
        command.env("FOUNDATION_THEMES_RUST_DIR", themes_rust_dir);
    }
    command.output().map_err(Into::into)
}

fn run_nix_codegen_check(
    project_root: &Path,
    sdk_root: &Path,
    package_name: &str,
    themes_rust_dir: Option<&Path>,
) -> Result<std::process::Output> {
    let mut command = Command::new("nix");
    command
        .current_dir(project_root)
        .arg("develop")
        .arg(sdk_root)
        .arg("--command")
        .arg("cargo")
        .arg("check")
        .arg("--quiet")
        .arg("--package")
        .arg(package_name);
    command.env(UI_LIBRARY_PATH_ENV, project_sdk_ui_root(project_root));
    command
        .env("FOUNDATION_THEMES_SLINT_DIR", crate::commands::themes::project_theme_slint_dir(project_root));
    if let Some(themes_rust_dir) = themes_rust_dir {
        command.env("FOUNDATION_THEMES_RUST_DIR", themes_rust_dir);
    }
    command.output().map_err(Into::into)
}

fn command_output_summary(label: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return format!("{label} failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return format!("{label} failed: {stdout}");
    }

    String::new()
}

fn extract_string_value(content: &str, prefix: &str) -> Option<String> {
    let start = content.find(prefix)? + prefix.len();
    let rest = &content[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_bool_value(content: &str, prefix: &str) -> Option<bool> {
    let start = content.find(prefix)? + prefix.len();
    let rest = &content[start..];
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

pub(crate) fn find_project_root(start: &Path) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    find_project_root_from(start, cwd.as_deref())
}

fn find_project_root_from(start: &Path, cwd: Option<&Path>) -> Option<PathBuf> {
    let start = if start.is_absolute() { start.to_path_buf() } else { cwd?.join(start) };
    let mut current = if start.is_file() { start.parent()?.to_path_buf() } else { start };

    loop {
        if current.join("Cargo.toml").exists() {
            return Some(current);
        }

        if !current.pop() {
            return None;
        }
    }
}

fn generated_slint_files_exist(project_root: &Path, options: &PreviewBuildOptions) -> bool {
    let gen_dir = project_root
        .join(&options.module_path)
        .parent()
        .map(|path| path.join("gen"))
        .unwrap_or_else(|| project_root.join("ui").join("gen"));

    if options.include_router
        && (!gen_dir.join("router.slint").exists() || !gen_dir.join("navigate.slint").exists())
    {
        return false;
    }

    if options.include_translations && !gen_dir.join("tr.slint").exists() {
        return false;
    }

    if (options.include_router || options.include_translations) && !gen_dir.join("exports.slint").exists() {
        return false;
    }

    options.include_router || options.include_translations
}

fn ensure_project_shared_tree_mapping(
    project_parent: &Path,
    project_target: &Path,
    sdk_source: &Path,
) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(project_target) {
        if metadata.file_type().is_symlink() {
            let existing = fs::read_link(project_target)?;
            let resolved = if existing.is_absolute() { existing } else { project_parent.join(existing) };

            if resolved == sdk_source {
                return Ok(());
            }

            fs::remove_file(project_target)?;
        } else if metadata.is_dir() {
            copy_dir_all(sdk_source, project_target)?;
            return Ok(());
        } else {
            anyhow::bail!("Expected {} to be a directory or symlink", project_target.display());
        }
    }

    fs::create_dir_all(project_parent)?;

    if create_dir_symlink(sdk_source, project_target).is_err() {
        copy_dir_all(sdk_source, project_target)?;
    }

    Ok(())
}

#[cfg(unix)]
fn create_dir_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn create_dir_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(source, target)
}

#[cfg(not(any(unix, windows)))]
fn create_dir_symlink(_source: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory symlinks are not supported on this platform",
    ))
}

fn copy_dir_all(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;

    let mut entries: Vec<_> = fs::read_dir(source)?.collect::<std::result::Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all(&path, &destination_path)?;
        } else {
            fs::copy(&path, &destination_path)?;
        }
    }

    Ok(())
}

#[derive(Debug)]
struct PreviewBuildOptions {
    module_path: String,
    include_router: bool,
    include_translations: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodegenMode {
    MissingOnly,
    AlwaysCheck,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use foundation_core::SdkRoot;
    use toml::Value;

    use super::{
        ensure_project_ui_mapping, generated_slint_files_exist, parse_build_options,
        prepare_project_for_build_with_theme_dir, prepare_slint_file_for_view,
        prepare_slint_file_for_view_from, project_sdk_ui_root,
    };
    use crate::sdk_mapping::project_sdk_keyos_root_path;
    use crate::test_support::{link_fake_bin, make_temp_dir};

    #[test]
    fn parses_preview_build_options_from_build_rs() {
        let root_dir = make_temp_dir("build-rs-parse");
        let root = root_dir.path();
        let build_rs = root.join("build.rs");
        fs::write(
            &build_rs,
            r#"
            fn main() {
                slint_keyos_platform_build::compile_options(slint_keyos_platform_build::CompileOptions {
                    module_path: "ui/custom-app.slint",
                    include_router: true,
                    include_slint: true,
                    include_translations: true,
                    include_time_localization: false,
                });
            }
            "#,
        )
        .unwrap();

        let options = parse_build_options(&build_rs).unwrap();
        assert_eq!(options.module_path, "ui/custom-app.slint");
        assert!(options.include_router);
        assert!(options.include_translations);
    }

    #[test]
    fn creates_project_ui_mapping_from_sdk_ui_snapshot() {
        let (_sdk_dir, sdk_root) = make_sdk_root("fresh-project");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let project_root_dir = make_temp_dir("project");
        let project_root = project_root_dir.path();

        ensure_project_ui_mapping(project_root, &sdk).unwrap();

        let project_ui = project_root.join("ui").join("ui");
        assert!(project_ui.exists());
        assert!(project_sdk_ui_root(project_root).exists());
        assert_eq!(fs::read_to_string(project_ui.join("theme.slint")).unwrap(), "theme\n");
        assert_eq!(fs::read_to_string(project_ui.join("button.slint")).unwrap(), "button\n");
        assert_eq!(
            fs::read_to_string(
                project_root
                    .join("target")
                    .join("foundation")
                    .join("resources")
                    .join("icons")
                    .join("loader.svg")
            )
            .unwrap(),
            "<svg>loader</svg>\n"
        );
        assert!(!project_root.join("resources").join("icons").exists());
    }

    #[test]
    fn refreshes_existing_project_ui_directory() {
        let (_sdk_dir, sdk_root) = make_sdk_root("existing-project");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let project_root_dir = make_temp_dir("project-existing");
        let project_root = project_root_dir.path();
        let project_ui = project_root.join("ui").join("ui");
        let project_ui_resources = project_root.join("resources");

        fs::create_dir_all(project_ui.join("icons")).unwrap();
        fs::create_dir_all(project_ui_resources.join("icons")).unwrap();
        fs::write(project_ui.join("theme.slint"), "old-theme\n").unwrap();
        fs::write(project_ui.join("button.slint"), "old-button\n").unwrap();
        fs::write(project_ui_resources.join("icons").join("loader.svg"), "app-loader\n").unwrap();

        ensure_project_ui_mapping(project_root, &sdk).unwrap();

        assert_eq!(fs::read_to_string(project_ui.join("theme.slint")).unwrap(), "theme\n");
        assert_eq!(fs::read_to_string(project_ui.join("button.slint")).unwrap(), "button\n");
        assert_eq!(
            fs::read_to_string(project_ui_resources.join("icons").join("loader.svg")).unwrap(),
            "app-loader\n"
        );
        assert_eq!(
            fs::read_to_string(
                project_root
                    .join("target")
                    .join("foundation")
                    .join("resources")
                    .join("icons")
                    .join("loader.svg")
            )
            .unwrap(),
            "app-loader\n"
        );
    }

    fn make_sdk_root(label: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = make_temp_dir(label);
        let repo_root = dir.path();
        fs::write(repo_root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let root = repo_root.join("sdk");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("flake.nix"), "{}").unwrap();
        fs::write(root.join("sdk-build.toml"), "").unwrap();
        link_fake_bin(&root.join("bin"));
        fs::create_dir_all(repo_root.join("ui2").join("components").join("ui")).unwrap();
        fs::create_dir_all(repo_root.join("ui2").join("resources").join("icons")).unwrap();
        fs::write(repo_root.join("ui2").join("components").join("ui").join("theme.slint"), "theme\n")
            .unwrap();
        fs::write(repo_root.join("ui2").join("components").join("ui").join("button.slint"), "button\n")
            .unwrap();
        fs::write(
            repo_root.join("ui2").join("resources").join("icons").join("loader.svg"),
            "<svg>loader</svg>\n",
        )
        .unwrap();
        (dir, root)
    }

    #[test]
    fn detects_when_generated_router_files_exist() {
        let project_root_dir = make_temp_dir("gen-detect");
        let project_root = project_root_dir.path();
        let gen_dir = project_root.join("ui").join("gen");
        fs::create_dir_all(&gen_dir).unwrap();
        fs::write(gen_dir.join("router.slint"), "").unwrap();
        fs::write(gen_dir.join("navigate.slint"), "").unwrap();
        fs::write(gen_dir.join("exports.slint"), "").unwrap();

        let options = super::PreviewBuildOptions {
            module_path: "ui/app.slint".to_string(),
            include_router: true,
            include_translations: false,
        };

        assert!(generated_slint_files_exist(project_root, &options));
    }

    #[test]
    fn prepare_project_for_build_materializes_ui_mapping_and_generated_files() {
        let (_sdk_dir, sdk_root) = make_sdk_root("build-project");
        let sdk = SdkRoot::from_root(sdk_root.clone()).unwrap();
        let (_proj_dir, project_root) = make_codegen_project("build-project");

        let themes_dir = project_root.join("themes-cache");
        prepare_project_for_build_with_theme_dir(&project_root, &sdk, Some(&themes_dir)).unwrap();

        let project_ui = project_root.join("ui").join("ui");
        assert!(project_ui.exists());
        assert!(project_root.join(project_sdk_keyos_root_path()).exists());
        assert_eq!(fs::read_to_string(project_ui.join("theme.slint")).unwrap(), "theme\n");

        let gen_dir = project_root.join("ui").join("gen");
        assert_eq!(fs::read_to_string(gen_dir.join("router.slint")).unwrap(), "v1\n");
        assert_eq!(fs::read_to_string(gen_dir.join("exports.slint")).unwrap(), "v1\n");
        assert_eq!(fs::read_to_string(gen_dir.join("tr.slint")).unwrap(), "v1\n");
        let manifest_toml = fs::read_to_string(project_root.join("manifest.toml")).unwrap();
        assert!(manifest_toml.contains("appId = \"0x00112233445566778899aabbccddeeff\""));
        assert!(manifest_toml.contains("en = \"Sample App\""));
        let manifest: Value = toml::from_str(&manifest_toml).unwrap();
        assert_eq!(
            manifest["permissions"]["os/gui-server"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            vec!["RegisterAppMessage", "RequestRedraw"]
        );
    }

    #[test]
    fn prepare_slint_file_for_view_refreshes_existing_generated_files() {
        let (_proj_dir, project_root) = make_codegen_project("view-project");
        let slint_file = project_root.join("ui").join("app.slint");
        let gen_dir = project_root.join("ui").join("gen");

        prepare_slint_file_for_view(&slint_file, None).unwrap();
        assert_eq!(fs::read_to_string(gen_dir.join("router.slint")).unwrap(), "v1\n");

        fs::write(project_root.join("stamp.txt"), "v2\n").unwrap();

        prepare_slint_file_for_view(&slint_file, None).unwrap();
        assert_eq!(fs::read_to_string(gen_dir.join("router.slint")).unwrap(), "v2\n");
        assert_eq!(fs::read_to_string(gen_dir.join("tr.slint")).unwrap(), "v2\n");
    }

    #[test]
    fn prepare_slint_file_for_view_supports_relative_paths_from_project_root() {
        let (_proj_dir, project_root) = make_codegen_project("relative-view-project");

        prepare_slint_file_for_view_from(Path::new("ui/app.slint"), None, Some(&project_root)).unwrap();

        let gen_dir = project_root.join("ui").join("gen");
        assert_eq!(fs::read_to_string(gen_dir.join("router.slint")).unwrap(), "v1\n");
        assert_eq!(fs::read_to_string(gen_dir.join("tr.slint")).unwrap(), "v1\n");
    }

    fn make_codegen_project(label: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = make_temp_dir(label);
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("ui")).unwrap();

        fs::write(
            root.join("Cargo.toml"),
            r#"
            [package]
            name = "sample-app"
            version = "0.1.0"
            edition = "2021"
            "#,
        )
        .unwrap();
        fs::write(
            root.join("app-config.toml"),
            r#"
            app-name = "sample-app"
            friendly-app-name = "Sample App"
            description = "Sample description"
            icon = "resources/icon.svg"
            app-id = "0x00112233445566778899aabbccddeeff"
            version = "0.1.0"
            min-keyos-version = "1.0.0"

            [permissions]
            template = ["gui-app"]
            "#,
        )
        .unwrap();
        fs::write(
            root.join("permission_templates.toml"),
            r#"
            [gui-app]
            "os/gui-server" = ["RegisterAppMessage", "RequestRedraw"]
            "#,
        )
        .unwrap();
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::write(root.join("resources").join("icon.svg"), r#"<svg width="96" height="96"></svg>"#).unwrap();

        fs::write(root.join("src").join("lib.rs"), "pub fn sample() {}\n").unwrap();
        fs::write(root.join("stamp.txt"), "v1\n").unwrap();
        fs::write(
            root.join("ui").join("app.slint"),
            r#"
            import { Router } from "./gen/router.slint";

            export component AppWindow {
                Router { }
            }

            export * from "gen/exports.slint";
            "#,
        )
        .unwrap();
        fs::write(
            root.join("build.rs"),
            r#"
            use std::path::PathBuf;

            fn main() {
                // module_path: "ui/app.slint"
                // include_router: true
                // include_translations: true
                println!("cargo:rerun-if-changed=stamp.txt");

                let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
                let stamp = std::fs::read_to_string(manifest_dir.join("stamp.txt")).unwrap();
                let gen_dir = manifest_dir.join("ui").join("gen");
                std::fs::create_dir_all(&gen_dir).unwrap();

                for file in ["router.slint", "navigate.slint", "internal.slint", "exports.slint", "tr.slint"] {
                    std::fs::write(gen_dir.join(file), &stamp).unwrap();
                }
            }
            "#,
        )
        .unwrap();

        (dir, root)
    }
}
