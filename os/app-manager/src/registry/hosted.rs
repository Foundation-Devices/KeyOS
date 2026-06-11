// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Path, PathBuf};

use super::{read_cosign2_header_from_reader, read_cosign2_version_from_reader, AppBinaryMetadata};

pub(super) fn app_icon_exists(path: &str) -> bool {
    let path = Path::new(path);
    path.is_file() || hosted_raw_icon_source_path(path).is_some()
}

pub(super) fn read_app_icon_bytes(path: &str, max_size_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let path = Path::new(path);
    match std::fs::metadata(path) {
        Ok(metadata) => {
            if metadata.len() > max_size_bytes {
                anyhow::bail!("app icon {} is too large: {} bytes", path.display(), metadata.len());
            }
            Ok(std::fs::read(path)?)
        }
        Err(_) if hosted_raw_icon_source_path(path).is_some() => {
            log::debug!("hosted app icon {} has a source image but no raw image", path.display());
            Ok(Vec::new())
        }
        Err(error) => Err(error.into()),
    }
}

fn hosted_raw_icon_source_path(path: &Path) -> Option<PathBuf> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("raw") {
        return None;
    }

    ["svg", "png", "jpg", "jpeg", "webp", "bmp"]
        .into_iter()
        .map(|extension| path.with_extension(extension))
        .find(|candidate| candidate.is_file())
}

pub(super) fn app_binary_metadata(elf_path: &str) -> AppBinaryMetadata {
    let size_bytes = app_binary_size(elf_path).unwrap_or_default();
    AppBinaryMetadata {
        version: std::fs::File::open(elf_path)
            .ok()
            .and_then(|mut file| read_cosign2_version_from_reader(&mut file))
            .unwrap_or_default(),
        size_bytes,
    }
}

pub(super) fn app_binary_size(elf_path: &str) -> anyhow::Result<u64> {
    Ok(std::fs::metadata(elf_path)?.len())
}

pub(super) fn read_app_header(elf_path: &str) -> anyhow::Result<Option<cosign2::Header>> {
    let mut file = std::fs::File::open(elf_path)?;
    Ok(read_cosign2_header_from_reader(&mut file))
}

pub(super) fn read_app_bytes(elf_path: &str) -> anyhow::Result<Vec<u8>> { Ok(std::fs::read(elf_path)?) }
