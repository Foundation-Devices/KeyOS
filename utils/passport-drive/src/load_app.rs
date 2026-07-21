// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde_json::Value;
use usb_debug_protocol::{
    Command, ProtocolError, UsbDebugClient, LOAD_APP_CHUNK_MAX, LOAD_APP_FILE_PATH_MAX,
};

pub(crate) struct LoadAppReport {
    pub app_id: String,
    pub elf_bytes: usize,
    pub manifest_bytes: usize,
    pub icon_bytes: Option<usize>,
    pub resource_files: usize,
    pub resource_bytes: usize,
}

/// Which sideload directory the bundle lands in on the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SideloadKind {
    /// A standard SDK app, launched directly from the system launcher.
    Standard,
    /// A Flux child app, run by the Flux emulator (Legacy mode).
    Flux,
}

impl SideloadKind {
    fn begin_command(self, app_id: [u8; 16]) -> Command {
        match self {
            SideloadKind::Standard => Command::LoadAppBegin { app_id },
            SideloadKind::Flux => Command::LoadFluxAppBegin { app_id },
        }
    }

    /// The on-device directory the bundle lands in, for reporting.
    pub(crate) fn device_dir(self) -> &'static str {
        match self {
            SideloadKind::Standard => "keyos/sideloaded-apps",
            SideloadKind::Flux => "keyos/apps/gui-app-emu-flux/sideloaded-apps",
        }
    }
}

/// Read a file from a bundle, refusing one that resolves outside the confinement root.
fn read_bundle_file(path: &Path) -> Result<Vec<u8>> {
    crate::check_jail(path)?;
    std::fs::read(path).with_context(|| format!("Cannot read {}", path.display()))
}

pub(crate) fn load_app(
    transport: &UsbDebugClient,
    app_path: &Path,
    kind: SideloadKind,
) -> Result<LoadAppReport> {
    crate::check_jail(app_path)?;
    let metadata =
        std::fs::metadata(app_path).with_context(|| format!("Cannot access {}", app_path.display()))?;
    ensure!(metadata.is_dir(), "{} is not a directory", app_path.display());

    let elf_path = app_path.join("app.elf");
    let manifest_path = app_path.join("manifest.json");
    let icon_path = app_path.join("icon.bin");
    let resources_path = app_path.join("resources");

    let elf = read_bundle_file(&elf_path)?;
    let manifest = read_bundle_file(&manifest_path)?;
    let icon = if icon_path.exists() { Some(read_bundle_file(&icon_path)?) } else { None };
    let resource_files = collect_resource_files(&resources_path)?;

    ensure!(!elf.is_empty(), "{} is empty", elf_path.display());
    ensure!(!manifest.is_empty(), "{} is empty", manifest_path.display());
    let app_id = validate_manifest_json(&manifest)?;
    warn_on_flux_mismatch(&manifest, kind);
    let app_id_bytes = decode_app_id(&app_id)?;

    send_ack(transport, kind.begin_command(app_id_bytes), "starting load_app", Duration::from_secs(5))?;
    upload_file(transport, "app.elf", &elf)?;
    upload_file(transport, "manifest.json", &manifest)?;
    if let Some(icon) = icon.as_deref() {
        upload_file(transport, "icon.bin", icon)?;
    }
    let mut resource_bytes = 0usize;
    for resource in &resource_files {
        let data = read_bundle_file(&resource.absolute_path)?;
        resource_bytes += data.len();
        upload_file(transport, &format!("resources/{}", resource.relative_path), &data)?;
    }
    send_ack(transport, Command::LoadAppEnd, "finishing load_app", Duration::from_secs(15))?;

    Ok(LoadAppReport {
        app_id,
        elf_bytes: elf.len(),
        manifest_bytes: manifest.len(),
        icon_bytes: icon.as_ref().map(Vec::len),
        resource_files: resource_files.len(),
        resource_bytes,
    })
}

fn upload_file(transport: &UsbDebugClient, filename: &str, data: &[u8]) -> Result<()> {
    ensure!(is_valid_upload_relative_path(filename), "Invalid upload path: {filename}");
    ensure!(filename.len() <= LOAD_APP_FILE_PATH_MAX, "Upload path is too long for usb-debug: {filename}");
    send_ack(
        transport,
        Command::LoadAppFileBegin { filename: filename.to_string(), size: data.len() as u64 },
        &format!("starting upload of {filename}"),
        Duration::from_secs(10),
    )?;
    for chunk in data.chunks(LOAD_APP_CHUNK_MAX) {
        send_ack(
            transport,
            Command::LoadAppChunk(chunk.to_vec()),
            &format!("uploading {filename}"),
            Duration::from_secs(10),
        )?;
    }
    Ok(())
}

fn send_ack(transport: &UsbDebugClient, cmd: Command, context: &str, timeout: Duration) -> Result<()> {
    match transport.send(cmd, timeout) {
        Ok(_) => Ok(()),
        Err(error) if matches!(error.downcast_ref::<ProtocolError>(), Some(ProtocolError::DeviceLocked)) => {
            Err(anyhow::anyhow!("Device rejected {context}: device is locked"))
        }
        Err(error) => Err(error).with_context(|| format!("Device rejected {context}")),
    }
}

fn validate_manifest_json(manifest: &[u8]) -> Result<String> {
    let json: Value =
        serde_json::from_slice(manifest_json(manifest)?).context("manifest.json is not valid JSON")?;
    let app_id =
        json.get("appId").and_then(Value::as_str).context("manifest.json is missing string field appId")?;
    ensure!(
        json.get("appName").and_then(Value::as_object).is_some(),
        "manifest.json is missing object field appName"
    );
    normalize_app_id(app_id)
}

/// The JSON inside the signed manifest. manifest.json is cosign2-signed, a header followed by the
/// JSON; the signed bytes are uploaded to the device verbatim and only this metadata read needs the
/// JSON. The device verifies the signature, so here we just drop the header.
fn manifest_json(manifest: &[u8]) -> Result<&[u8]> {
    manifest
        .get(cosign2::Header::DEFAULT_SIZE..)
        .context("manifest.json is too short to hold a cosign2 header")
}

/// Warn when `--flux` disagrees with the bundle's manifest. A Flux app's manifest declares the
/// `os/gui-app-emu-flux` server, so uploading it without `--flux` (or a non-Flux app with
/// `--flux`) lands it in the wrong directory and is almost certainly a mistake. The flag and the
/// manifest are a hidden dependency, so surface the mismatch rather than silently proceed.
fn warn_on_flux_mismatch(manifest: &[u8], kind: SideloadKind) {
    let Ok(json) = manifest_json(manifest)
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).context("manifest.json is not valid JSON"))
    else {
        return;
    };
    let declares_emulator =
        json.get("permissions").and_then(|perms| perms.get("os/gui-app-emu-flux")).is_some();
    match (kind, declares_emulator) {
        (SideloadKind::Standard, true) => eprintln!(
            "Warning: this bundle declares the Flux emulator server (os/gui-app-emu-flux) but --flux \
             was not passed; it will land in {} and the Flux emulator won't see it. Did you mean --flux?",
            SideloadKind::Standard.device_dir()
        ),
        (SideloadKind::Flux, false) => eprintln!(
            "Warning: --flux was passed but this bundle does not declare the Flux emulator server \
             (os/gui-app-emu-flux); it may not be a Flux app."
        ),
        _ => {}
    }
}

fn normalize_app_id(app_id: &str) -> Result<String> {
    let app_id_bytes = app_manifest::parse_app_id_bytes(app_id)
        .context("manifest.json appId must be lowercase 0x-prefixed 16-byte hex")?;
    Ok(hex::encode(app_id_bytes))
}

fn decode_app_id(app_id: &str) -> Result<[u8; 16]> {
    let bytes = hex::decode(app_id).context("manifest.json appId is not valid hex")?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!("manifest.json appId must be 16 bytes of hex, got {} bytes", bytes.len())
    })
}

struct ResourceFile {
    absolute_path: PathBuf,
    relative_path: String,
}

fn collect_resource_files(resources_path: &Path) -> Result<Vec<ResourceFile>> {
    if !resources_path.exists() {
        return Ok(Vec::new());
    }
    ensure!(resources_path.is_dir(), "{} is not a directory", resources_path.display());

    let mut files = Vec::new();
    collect_resource_files_inner(resources_path, resources_path, &mut files)?;
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(files)
}

fn collect_resource_files_inner(root: &Path, dir: &Path, files: &mut Vec<ResourceFile>) -> Result<()> {
    crate::check_jail(dir)?;
    for entry in std::fs::read_dir(dir).with_context(|| format!("Cannot list {}", dir.display()))? {
        let entry = entry.with_context(|| format!("Cannot read entry in {}", dir.display()))?;
        let path = entry.path();
        let metadata = std::fs::metadata(&path)
            .with_context(|| format!("Cannot read metadata for {}", path.display()))?;
        if metadata.is_dir() {
            collect_resource_files_inner(root, &path, files)?;
        } else if metadata.is_file() {
            let relative_path = path.strip_prefix(root).expect("resource path is under root");
            let mut parts = Vec::new();
            for component in relative_path.components() {
                match component {
                    Component::Normal(part) => {
                        let part = part.to_str().with_context(|| {
                            format!("Resource path is not valid UTF-8: {}", relative_path.display())
                        })?;
                        parts.push(part);
                    }
                    _ => anyhow::bail!("Invalid resource path: {}", relative_path.display()),
                }
            }
            let relative_path = parts.join("/");
            ensure!(is_valid_upload_relative_path(&relative_path), "Invalid resource path: {relative_path}");
            ensure!(
                "resources/".len() + relative_path.len() <= LOAD_APP_FILE_PATH_MAX,
                "Resource path is too long for usb-debug: resources/{relative_path}"
            );
            files.push(ResourceFile { absolute_path: path, relative_path });
        }
    }
    Ok(())
}

fn is_valid_upload_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_APP_ID: &str = "0x00112233445566778899aabbccddeeff";
    const VALID_APP_ID_HEX: &str = "00112233445566778899aabbccddeeff";

    /// A manifest as built: an opaque cosign2 header followed by the JSON.
    fn manifest_with_app_id(app_id: &str) -> Vec<u8> {
        let json = format!(r#"{{"appName":{{"en":"Test"}},"appId":"{app_id}"}}"#);
        let mut bundle = vec![0u8; cosign2::Header::DEFAULT_SIZE];
        bundle.extend_from_slice(json.as_bytes());
        bundle
    }

    #[test]
    fn manifest_app_id_accepts_device_format() {
        let app_id = validate_manifest_json(&manifest_with_app_id(VALID_APP_ID)).unwrap();

        assert_eq!(app_id, VALID_APP_ID_HEX);
    }

    #[test]
    fn manifest_without_cosign2_header_is_rejected() {
        let error = validate_manifest_json(br#"{"appName":{"en":"Test"},"appId":"0x00"}"#).unwrap_err();

        assert!(error.to_string().contains("too short to hold a cosign2 header"));
    }

    #[test]
    fn manifest_app_id_accepts_uppercase_hex_digits() {
        let app_id =
            validate_manifest_json(&manifest_with_app_id("0x00112233445566778899AABBCCDDEEFF")).unwrap();

        assert_eq!(app_id, VALID_APP_ID_HEX);
    }

    #[test]
    fn manifest_app_id_rejects_bare_hex() {
        let error = validate_manifest_json(&manifest_with_app_id(VALID_APP_ID_HEX)).unwrap_err();

        assert!(error.to_string().contains("lowercase 0x-prefixed"));
    }

    #[test]
    fn manifest_app_id_rejects_uppercase_prefix() {
        let error =
            validate_manifest_json(&manifest_with_app_id("0X00112233445566778899aabbccddeeff")).unwrap_err();

        assert!(error.to_string().contains("lowercase 0x-prefixed"));
    }
}
