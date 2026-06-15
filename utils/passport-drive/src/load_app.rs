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

pub(crate) fn load_app(transport: &UsbDebugClient, app_path: &Path) -> Result<LoadAppReport> {
    let metadata =
        std::fs::metadata(app_path).with_context(|| format!("Cannot access {}", app_path.display()))?;
    ensure!(metadata.is_dir(), "{} is not a directory", app_path.display());

    let elf_path = app_path.join("app.elf");
    let manifest_path = app_path.join("manifest.json");
    let icon_path = app_path.join("icon.bin");
    let resources_path = app_path.join("resources");

    let elf = std::fs::read(&elf_path).with_context(|| format!("Cannot read {}", elf_path.display()))?;
    let manifest =
        std::fs::read(&manifest_path).with_context(|| format!("Cannot read {}", manifest_path.display()))?;
    let icon = if icon_path.exists() {
        Some(std::fs::read(&icon_path).with_context(|| format!("Cannot read {}", icon_path.display()))?)
    } else {
        None
    };
    let resource_files = collect_resource_files(&resources_path)?;

    ensure!(!elf.is_empty(), "{} is empty", elf_path.display());
    ensure!(!manifest.is_empty(), "{} is empty", manifest_path.display());
    let app_id = validate_manifest_json(&manifest)?;
    let app_id_bytes = decode_app_id(&app_id)?;

    send_ack(
        transport,
        Command::LoadAppBegin { app_id: app_id_bytes },
        "starting load_app",
        Duration::from_secs(5),
    )?;
    upload_file(transport, "app.elf", &elf)?;
    upload_file(transport, "manifest.json", &manifest)?;
    if let Some(icon) = icon.as_deref() {
        upload_file(transport, "icon.bin", icon)?;
    }
    let mut resource_bytes = 0usize;
    for resource in &resource_files {
        let data = std::fs::read(&resource.absolute_path)
            .with_context(|| format!("Cannot read {}", resource.absolute_path.display()))?;
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
    let json: Value = serde_json::from_slice(manifest).context("manifest.json is not valid JSON")?;
    let app_id =
        json.get("appId").and_then(Value::as_str).context("manifest.json is missing string field appId")?;
    ensure!(
        json.get("appName").and_then(Value::as_object).is_some(),
        "manifest.json is missing object field appName"
    );
    normalize_app_id(app_id)
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
    for entry in std::fs::read_dir(dir).with_context(|| format!("Cannot list {}", dir.display()))? {
        let entry = entry.with_context(|| format!("Cannot read entry in {}", dir.display()))?;
        let path = entry.path();
        let metadata =
            entry.metadata().with_context(|| format!("Cannot read metadata for {}", path.display()))?;
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

    fn manifest_with_app_id(app_id: &str) -> Vec<u8> {
        format!(r#"{{"appName":{{"en":"Test"}},"appId":"{app_id}"}}"#).into_bytes()
    }

    #[test]
    fn manifest_app_id_accepts_device_format() {
        let app_id = validate_manifest_json(&manifest_with_app_id(VALID_APP_ID)).unwrap();

        assert_eq!(app_id, VALID_APP_ID_HEX);
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
