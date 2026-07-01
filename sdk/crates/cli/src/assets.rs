// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};
use foundation_core::{AppConfig, SdkRoot};

pub const BUNDLED_ICON_FILE: &str = "icon.bin";

const ASSET_TOOL_BINARY: &str = "foundation-asset-tool";
const ASSET_TOOL_ENV: &str = "FOUNDATION_ASSET_TOOL";
const RESOURCES_DIR: &str = "resources";
const LEGACY_BUILD_ASSETS_DIR: &str = "assets";
const IMAGES_DIR: &str = "images";
const FONTS_DIR: &str = "fonts";

pub fn stage_hardware_assets(config: &AppConfig, project_root: &Path, output_dir: &Path) -> Result<PathBuf> {
    stage_hardware_assets_with_tool(config, project_root, output_dir, &AssetTool::resolve()?)
}

fn stage_hardware_assets_with_tool(
    config: &AppConfig,
    project_root: &Path,
    output_dir: &Path,
    tool: &AssetTool,
) -> Result<PathBuf> {
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

    stage_bundled_icon_with_tool(config, project_root, output_dir, tool)?;
    stage_images(project_root, &resources_dir, tool)?;
    stage_fonts(project_root, &resources_dir)?;

    Ok(resources_dir)
}

/// Stage the single bundled icon next to app.elf. Resolves the asset tool on its
/// own; callers staging a full asset set should go through stage_hardware_assets
/// so the resolve happens once.
pub fn stage_bundled_icon(config: &AppConfig, project_root: &Path, output_dir: &Path) -> Result<()> {
    stage_bundled_icon_with_tool(config, project_root, output_dir, &AssetTool::resolve()?)
}

fn stage_bundled_icon_with_tool(
    config: &AppConfig,
    project_root: &Path,
    output_dir: &Path,
    tool: &AssetTool,
) -> Result<()> {
    let icon_source = project_root.join(&config.icon);
    let icon_destination = output_dir.join(BUNDLED_ICON_FILE);
    convert_image_file(&icon_source, &icon_destination, tool)
}

fn stage_images(project_root: &Path, resources_dir: &Path, tool: &AssetTool) -> Result<()> {
    let output_root = resources_dir.join(IMAGES_DIR);

    stage_image_source_dir(&project_root.join(RESOURCES_DIR).join(IMAGES_DIR), &output_root, tool)?;
    stage_image_source_dir(&project_root.join(IMAGES_DIR), &output_root, tool)
}

fn stage_image_source_dir(images_dir: &Path, output_root: &Path, tool: &AssetTool) -> Result<()> {
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
        convert_image_to_raw_dir(&image_path, &output_root.join(relative_parent), tool)?;
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

fn convert_image_file(source: &Path, destination: &Path, tool: &AssetTool) -> Result<()> {
    let output = tool
        .command()
        .arg("raw-image-file")
        .arg(source)
        .arg(destination)
        .output()
        .with_context(|| format!("Failed to run {ASSET_TOOL_BINARY}"))?;
    ensure_asset_tool_success(source, &output)?;
    Ok(())
}

fn convert_image_to_raw_dir(source: &Path, destination_dir: &Path, tool: &AssetTool) -> Result<()> {
    let output = tool
        .command()
        .arg("raw-image-dir")
        .arg(source)
        .arg(destination_dir)
        .output()
        .with_context(|| format!("Failed to run {ASSET_TOOL_BINARY}"))?;
    ensure_asset_tool_success(source, &output)?;
    Ok(())
}

/// A resolved invocation of the asset-conversion helper. Resolved once per
/// staging run so the lookup (env override, SDK tree, sibling binary, source
/// build) happens a single time rather than per converted asset.
enum AssetTool {
    Binary(PathBuf),
    CargoManifest(PathBuf),
}

impl AssetTool {
    fn resolve() -> Result<Self> {
        if let Some(path) = std::env::var_os(ASSET_TOOL_ENV) {
            return Ok(Self::Binary(path.into()));
        }

        if let Ok(sdk) = SdkRoot::discover() {
            if let Some(path) = sdk.tool_path(&[ASSET_TOOL_BINARY]) {
                return Ok(Self::Binary(path));
            }
        }

        if let Some(path) = current_exe_sibling_asset_tool() {
            return Ok(Self::Binary(path));
        }

        let source_manifest = source_asset_tool_manifest();
        if source_manifest.exists() {
            return Ok(Self::CargoManifest(source_manifest));
        }

        bail!(
            "{ASSET_TOOL_BINARY} not found. Reinstall the Foundation SDK or set {ASSET_TOOL_ENV} to the helper binary path."
        )
    }

    fn command(&self) -> Command {
        match self {
            Self::Binary(path) => Command::new(path),
            Self::CargoManifest(manifest) => {
                let mut command = Command::new("cargo");
                command.arg("run").arg("--quiet").arg("--manifest-path").arg(manifest).arg("--");
                command
            }
        }
    }
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
    use std::sync::OnceLock;
    use std::time::{SystemTime, UNIX_EPOCH};

    use foundation_core::{AppId, PermissionsConfig, PublisherConfig};
    use semver::Version;

    use super::{stage_hardware_assets_with_tool, AssetTool};
    use crate::assets::{is_supported_font, is_supported_image};

    const ASSET_TOOL_STUB: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo 'foundation-asset-tool 1.0.0-fake'
  exit 0
fi
cmd="$1"
source="$2"
destination="$3"
case "$cmd" in
  raw-image-file)
    mkdir -p "$(dirname "$destination")"
    cp "$source" "$destination"
    ;;
  raw-image-dir)
    mkdir -p "$destination"
    base=$(basename "$source")
    stem=${base%.*}
    cp "$source" "$destination/$stem.raw"
    ;;
esac
"#;

    // A stub script standing in for the conversion helper, so the test stays in
    // process and never reaches for the unbuilt helper binary.
    fn asset_tool_stub() -> AssetTool {
        static STUB: OnceLock<PathBuf> = OnceLock::new();
        let script = STUB.get_or_init(|| {
            let dir = make_temp_dir("asset-tool-stub");
            let script = dir.join("foundation-asset-tool");
            fs::write(&script, ASSET_TOOL_STUB).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&script).unwrap().permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&script, perms).unwrap();
            }
            script
        });
        AssetTool::Binary(script.clone())
    }

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
        let resources_dir =
            stage_hardware_assets_with_tool(&app_config(), &root, &output_dir, &asset_tool_stub()).unwrap();

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
        let resources_dir =
            stage_hardware_assets_with_tool(&app_config(), &root, &output_dir, &asset_tool_stub()).unwrap();

        assert!(output_dir.join("icon.bin").exists());
        assert!(!resources_dir.join("images").join("sample.raw").exists());

        cleanup(&root);
        cleanup(&sdk_shared);
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
