// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};
use foundation_core::{AppConfig, SdkRoot};

pub const APP_RESOURCES_DIR_ENV: &str = "FOUNDATION_APP_RESOURCES_DIR";
pub const BUNDLED_ICON_FILE: &str = "icon.bin";

const ASSET_TOOL_BINARY: &str = "foundation-asset-tool";
const ASSET_TOOL_ENV: &str = "FOUNDATION_ASSET_TOOL";
const RESOURCES_DIR: &str = "resources";
const LEGACY_BUILD_ASSETS_DIR: &str = "assets";
const IMAGES_DIR: &str = "images";
const FONTS_DIR: &str = "fonts";

pub fn stage_hardware_assets(config: &AppConfig, project_root: &Path, output_dir: &Path) -> Result<PathBuf> {
    let resources_dir = output_dir.join(RESOURCES_DIR);
    let legacy_assets_dir = output_dir.join(LEGACY_BUILD_ASSETS_DIR);

    if resources_dir.exists() {
        fs::remove_dir_all(&resources_dir).with_context(|| {
            format!("Failed to clean app resources directory {}", resources_dir.display())
        })?;
    }
    if legacy_assets_dir.exists() {
        fs::remove_dir_all(&legacy_assets_dir).with_context(|| {
            format!("Failed to clean legacy generated asset directory {}", legacy_assets_dir.display())
        })?;
    }

    fs::create_dir_all(&resources_dir)
        .with_context(|| format!("Failed to create app resources directory {}", resources_dir.display()))?;

    stage_icon(config, project_root, output_dir)?;
    stage_bundled_icon(config, project_root, output_dir)?;
    stage_images(project_root, &resources_dir)?;
    stage_fonts(project_root, &resources_dir)?;

    Ok(resources_dir)
}

pub fn stage_simulator_resources(config: &AppConfig, project_root: &Path) -> Result<PathBuf> {
    let staged_resources = project_root.join("target").join("foundation").join("sim-resources");

    if staged_resources.exists() {
        fs::remove_dir_all(&staged_resources).with_context(|| {
            format!("Failed to clean simulator resources directory {}", staged_resources.display())
        })?;
    }
    fs::create_dir_all(&staged_resources).with_context(|| {
        format!("Failed to create simulator resources directory {}", staged_resources.display())
    })?;

    let resources = project_root.join(RESOURCES_DIR);
    if resources.exists() {
        copy_dir_contents(&resources, &staged_resources)?;
    }

    stage_simulator_app_asset_dirs(project_root, &staged_resources)?;
    stage_simulator_icon(config, project_root, &staged_resources)?;

    Ok(staged_resources)
}

pub fn copy_app_resources_to_bundle(resources_dir: &Path, install_dir: &Path) -> Result<()> {
    if !resources_dir.exists() {
        return Ok(());
    }

    let install_resources = install_dir.join(RESOURCES_DIR);
    if let Ok(metadata) = fs::symlink_metadata(&install_resources) {
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&install_resources).with_context(|| {
                format!("Failed to clean app resources directory {}", install_resources.display())
            })?;
        } else {
            fs::remove_file(&install_resources).with_context(|| {
                format!("Failed to replace app resources path {}", install_resources.display())
            })?;
        }
    }
    fs::create_dir_all(&install_resources).with_context(|| {
        format!("Failed to create app resources directory {}", install_resources.display())
    })?;
    copy_dir_contents(resources_dir, &install_resources)
}

fn stage_icon(config: &AppConfig, project_root: &Path, output_dir: &Path) -> Result<()> {
    let icon_source = project_root.join(&config.icon);
    let icon_destination = output_dir.join(config.manifest_icon_file());
    convert_image_file(&icon_source, &icon_destination)
}

fn stage_bundled_icon(config: &AppConfig, project_root: &Path, output_dir: &Path) -> Result<()> {
    let icon_source = project_root.join(&config.icon);
    let icon_destination = output_dir.join(BUNDLED_ICON_FILE);
    convert_image_file(&icon_source, &icon_destination)
}

fn stage_simulator_icon(config: &AppConfig, project_root: &Path, resources_dir: &Path) -> Result<()> {
    let icon_source = project_root.join(&config.icon);
    let extension = icon_source.extension().and_then(|extension| extension.to_str()).unwrap_or("png");
    let manifest_icon_name = config.manifest_icon_image_name();
    let icon_name =
        manifest_icon_name.strip_prefix("resources/").map(ToOwned::to_owned).unwrap_or(manifest_icon_name);
    let icon_destination = resources_dir.join(icon_name).with_extension(extension);
    write_file(&icon_destination, &fs::read(&icon_source)?)?;
    Ok(())
}

fn stage_images(project_root: &Path, resources_dir: &Path) -> Result<()> {
    let output_root = resources_dir.join(IMAGES_DIR);

    stage_image_source_dir(&project_root.join(RESOURCES_DIR).join(IMAGES_DIR), &output_root)?;
    stage_image_source_dir(&project_root.join(IMAGES_DIR), &output_root)
}

fn stage_image_source_dir(images_dir: &Path, output_root: &Path) -> Result<()> {
    if should_skip_asset_source_dir(&images_dir)? {
        return Ok(());
    }

    for image_path in collect_files(&images_dir)? {
        if !is_supported_image(&image_path) {
            continue;
        }

        let relative_parent = image_path
            .parent()
            .and_then(|parent| parent.strip_prefix(&images_dir).ok())
            .unwrap_or_else(|| Path::new(""));
        convert_image_to_raw_dir(&image_path, &output_root.join(relative_parent))?;
    }

    Ok(())
}

fn stage_fonts(project_root: &Path, resources_dir: &Path) -> Result<()> {
    let output_dir = resources_dir.join(FONTS_DIR);

    stage_font_source_dir(&project_root.join(RESOURCES_DIR).join(FONTS_DIR), &output_dir)?;
    stage_font_source_dir(&project_root.join(FONTS_DIR), &output_dir)
}

fn stage_font_source_dir(fonts_dir: &Path, output_dir: &Path) -> Result<()> {
    if should_skip_asset_source_dir(&fonts_dir)? {
        return Ok(());
    }

    for font_path in collect_files(&fonts_dir)? {
        if !is_supported_font(&font_path) {
            continue;
        }

        let relative = font_path
            .strip_prefix(&fonts_dir)
            .with_context(|| format!("Failed to compute relative font path for {}", font_path.display()))?;
        write_file(&output_dir.join(relative), &fs::read(&font_path)?)?;
    }

    Ok(())
}

fn stage_simulator_app_asset_dirs(project_root: &Path, staged_resources: &Path) -> Result<()> {
    copy_optional_asset_dir(&project_root.join(IMAGES_DIR), &staged_resources.join(IMAGES_DIR))?;
    copy_optional_asset_dir(&project_root.join(FONTS_DIR), &staged_resources.join(FONTS_DIR))
}

fn copy_optional_asset_dir(source: &Path, destination: &Path) -> Result<()> {
    if should_skip_asset_source_dir(source)? {
        return Ok(());
    }

    copy_dir_contents(source, destination)
}

fn convert_image_file(source: &Path, destination: &Path) -> Result<()> {
    let output = asset_tool_command()?
        .arg("raw-image-file")
        .arg(source)
        .arg(destination)
        .output()
        .with_context(|| format!("Failed to run {ASSET_TOOL_BINARY}"))?;
    ensure_asset_tool_success(source, &output)?;
    Ok(())
}

fn convert_image_to_raw_dir(source: &Path, destination_dir: &Path) -> Result<()> {
    let output = asset_tool_command()?
        .arg("raw-image-dir")
        .arg(source)
        .arg(destination_dir)
        .output()
        .with_context(|| format!("Failed to run {ASSET_TOOL_BINARY}"))?;
    ensure_asset_tool_success(source, &output)?;
    Ok(())
}

fn asset_tool_command() -> Result<Command> {
    if let Some(path) = std::env::var_os(ASSET_TOOL_ENV) {
        return Ok(Command::new(path));
    }

    if let Ok(sdk) = SdkRoot::discover() {
        if let Some(path) = sdk.tool_path(&[ASSET_TOOL_BINARY]) {
            return Ok(Command::new(path));
        }
    }

    if let Some(path) = current_exe_sibling_asset_tool() {
        return Ok(Command::new(path));
    }

    let source_manifest = source_asset_tool_manifest();
    if source_manifest.exists() {
        let mut command = Command::new("cargo");
        command.arg("run").arg("--quiet").arg("--manifest-path").arg(source_manifest).arg("--");
        return Ok(command);
    }

    bail!(
        "{ASSET_TOOL_BINARY} not found. Reinstall the Foundation SDK or set {ASSET_TOOL_ENV} to the helper binary path."
    )
}

fn current_exe_sibling_asset_tool() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    let current_dir = current_exe.parent()?;
    for dir in [Some(current_dir), current_dir.parent()] {
        let sibling = dir?.join(asset_tool_binary_name());
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    None
}

fn source_asset_tool_manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tools")
        .join(ASSET_TOOL_BINARY)
        .join("Cargo.toml")
}

fn asset_tool_binary_name() -> String {
    if std::env::consts::EXE_SUFFIX.is_empty() {
        ASSET_TOOL_BINARY.to_string()
    } else {
        format!("{}{}", ASSET_TOOL_BINARY, std::env::consts::EXE_SUFFIX)
    }
}

fn ensure_asset_tool_success(source: &Path, output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }

    bail!("Failed to convert image {}: {}", source.display(), asset_tool_output_message(output))
}

fn asset_tool_output_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }

    format!("asset tool exited with {}", output.status)
}

fn should_skip_asset_source_dir(path: &Path) -> Result<bool> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(true);
    };

    if metadata.file_type().is_symlink() {
        return Ok(true);
    }

    if !metadata.is_dir() {
        anyhow::bail!("Expected {} to be a directory", path.display());
    }

    Ok(false)
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_files_into(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files_into(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_files_into(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

fn copy_dir_contents(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;

    let mut entries = fs::read_dir(source)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_contents(&source_path, &destination_path)?;
        } else if source_path.is_file() {
            fs::create_dir_all(destination_path.parent().unwrap())?;
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!("Failed to copy {} to {}", source_path.display(), destination_path.display())
            })?;
            fs::File::open(&destination_path)
                .and_then(|file| file.sync_all())
                .with_context(|| format!("Failed to sync copied asset {}", destination_path.display()))?;
        }
    }

    Ok(())
}

fn write_file(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create asset output directory {}", parent.display()))?;
    }

    fs::write(path, contents).with_context(|| format!("Failed to write asset {}", path.display()))
}

fn is_supported_image(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("png" | "jpg" | "jpeg" | "svg" | "webp" | "bmp")
    )
}

fn is_supported_font(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("ttf" | "otf" | "ttc")
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use foundation_core::{AppId, PermissionsConfig, PublisherConfig};
    use semver::Version;

    use super::{copy_app_resources_to_bundle, stage_hardware_assets, stage_simulator_resources};
    use crate::assets::{is_supported_font, is_supported_image};

    #[test]
    fn stages_icon_images_and_fonts_for_hardware() {
        let root = make_temp_dir("hardware-assets");
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::create_dir_all(root.join("images").join("nested")).unwrap();
        fs::create_dir_all(root.join("fonts")).unwrap();
        fs::write(root.join("resources").join("icon.svg"), svg()).unwrap();
        fs::write(root.join("images").join("nested").join("photo.svg"), svg()).unwrap();
        fs::write(root.join("fonts").join("Brand.ttf"), b"font").unwrap();

        let output_dir = root.join("target").join("keyos").join("demo-app");
        let resources_dir = stage_hardware_assets(&app_config(), &root, &output_dir).unwrap();

        assert!(resources_dir.join(".foundation").join("icon.raw").exists());
        assert!(output_dir.join("icon.bin").exists());
        assert!(resources_dir.join("images").join("nested").join("photo.raw").exists());
        assert_eq!(fs::read(resources_dir.join("fonts").join("Brand.ttf")).unwrap(), b"font");

        cleanup(&root);
    }

    #[test]
    fn skips_sdk_shared_resource_symlink_dirs() {
        let root = make_temp_dir("symlink-assets");
        let sdk_shared = make_temp_dir("sdk-shared-assets");
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::create_dir_all(sdk_shared.join("images")).unwrap();
        fs::write(root.join("resources").join("icon.svg"), svg()).unwrap();
        create_dir_symlink(&sdk_shared.join("images"), &root.join("resources").join("images")).unwrap();

        let output_dir = root.join("target").join("keyos").join("demo-app");
        let resources_dir = stage_hardware_assets(&app_config(), &root, &output_dir).unwrap();

        assert!(resources_dir.join(".foundation").join("icon.raw").exists());
        assert!(output_dir.join("icon.bin").exists());
        assert!(!resources_dir.join("images").join("sample.raw").exists());

        cleanup(&root);
        cleanup(&sdk_shared);
    }

    #[test]
    fn simulator_resources_are_copied_to_a_generated_target_dir() {
        let root = make_temp_dir("sim-assets");
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::create_dir_all(root.join("images")).unwrap();
        fs::create_dir_all(root.join("fonts")).unwrap();
        fs::write(root.join("resources").join("icon.svg"), svg()).unwrap();
        fs::write(root.join("images").join("photo.svg"), svg()).unwrap();
        fs::write(root.join("fonts").join("Brand.ttf"), b"font").unwrap();

        let staged = stage_simulator_resources(&app_config(), &root).unwrap();

        assert_eq!(fs::read_to_string(staged.join("images").join("photo.svg")).unwrap(), svg());
        assert_eq!(fs::read(staged.join("fonts").join("Brand.ttf")).unwrap(), b"font");
        assert_eq!(fs::read_to_string(staged.join(".foundation").join("icon.svg")).unwrap(), svg());
        cleanup(&root);
    }

    #[test]
    fn copies_app_resources_into_bundle_resources_dir() {
        let root = make_temp_dir("copy-resources");
        let resources = root.join("target").join("keyos").join("demo-app").join("resources");
        fs::create_dir_all(resources.join("images")).unwrap();
        fs::write(resources.join("images").join("app.raw"), b"raw").unwrap();
        let install_dir = root.join("mount").join("keyos").join("apps").join("demo-app");

        copy_app_resources_to_bundle(&resources, &install_dir).unwrap();

        assert_eq!(fs::read(install_dir.join("resources").join("images").join("app.raw")).unwrap(), b"raw");
        cleanup(&root);
    }

    #[test]
    fn recognizes_supported_asset_extensions() {
        assert!(is_supported_image(Path::new("logo.svg")));
        assert!(is_supported_image(Path::new("photo.PNG")));
        assert!(!is_supported_image(Path::new("notes.txt")));
        assert!(is_supported_font(Path::new("Brand.ttf")));
        assert!(!is_supported_font(Path::new("Brand.txt")));
    }

    fn app_config() -> foundation_core::AppConfig {
        foundation_core::AppConfig {
            app_name: "demo-app".to_string(),
            friendly_app_name: "Demo App".to_string(),
            launcher_app_name: None,
            description: "Demo".to_string(),
            publisher: PublisherConfig::default(),
            icon: PathBuf::from("resources/icon.svg"),
            theme: None,
            app_id: AppId::from_hex("0x00112233445566778899aabbccddeeff").unwrap(),
            permissions: PermissionsConfig::default(),
            version: Version::parse("0.1.0").unwrap(),
            min_keyos_version: Version::parse("1.0.0").unwrap(),
            signing_identity: None,
            cosign2_config: None,
        }
    }

    fn svg() -> &'static str {
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#ff0000"/></svg>"##
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("foundation-assets-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn cleanup(path: &Path) { let _ = fs::remove_dir_all(path); }

    #[cfg(unix)]
    fn create_dir_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(source, target)
    }

    #[cfg(windows)]
    fn create_dir_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(source, target)
    }
}
