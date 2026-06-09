// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Read;

use super::{read_cosign2_header_from_reader, read_cosign2_version_from_reader, AppBinaryMetadata};
use crate::{CryptoApi, FileSystem};

pub(super) fn app_icon_exists(path: &str) -> bool {
    FileSystem::default().metadata(path, fs::Location::System).is_ok()
}

pub(super) fn read_app_icon_bytes(path: &str, max_size_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let fs = FileSystem::default();
    let metadata = fs.metadata(path, fs::Location::System)?;
    if metadata.size > max_size_bytes {
        anyhow::bail!("app icon {path} is too large: {} bytes", metadata.size);
    }

    let mut file = fs.open_file(path, fs::Location::System, fs::OpenFlags::READ_ONLY)?;
    let mut bytes = Vec::with_capacity(metadata.size as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn app_binary_metadata(elf_path: &str) -> AppBinaryMetadata {
    let fs = FileSystem::default();
    let size_bytes = app_binary_size(elf_path).unwrap_or_else(|e| {
        log::warn!("failed to read app file size for {elf_path}: {e:?}");
        0
    });

    AppBinaryMetadata {
        version: read_cosign2_version(&fs, elf_path, size_bytes).unwrap_or_default(),
        size_bytes,
    }
}

pub(super) fn app_binary_size(elf_path: &str) -> anyhow::Result<u64> {
    Ok(FileSystem::default().metadata(elf_path, fs::Location::System)?.size)
}

fn read_cosign2_version(fs: &FileSystem, elf_path: &str, size_bytes: u64) -> Option<String> {
    if size_bytes < cosign2::Header::DEFAULT_SIZE as u64 {
        return None;
    }

    let mut elf_file = fs
        .open_file(elf_path, fs::Location::System, fs::OpenFlags { read: true, write: false, create: false })
        .inspect_err(|e| log::warn!("failed to open app file for version read {elf_path}: {e:?}"))
        .ok()?;
    read_cosign2_version_from_reader(&mut elf_file)
}

pub(super) fn read_app_header(elf_path: &str) -> anyhow::Result<Option<cosign2::Header>> {
    let fs = FileSystem::default();
    let mut elf_file = fs
        .open_file(elf_path, fs::Location::System, fs::OpenFlags { read: true, write: false, create: false })
        .inspect_err(|e| log::warn!("failed to open app file for third-party key check {elf_path}: {e:?}"))?;
    Ok(read_cosign2_header_from_reader(&mut elf_file))
}

pub(super) fn read_app_bytes(elf_path: &str) -> anyhow::Result<Vec<u8>> {
    let fs = FileSystem::default();
    let metadata = fs.metadata(elf_path, fs::Location::System)?;
    let mut elf_file = fs
        .open_file(elf_path, fs::Location::System, fs::OpenFlags { read: true, write: false, create: false })
        .inspect_err(|e| log::warn!("failed to open app file for third-party key check {elf_path}: {e:?}"))?;
    let mut bytes = vec![0; metadata.size as usize];
    elf_file.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn verify_third_party_app_header(
    bytes: &[u8],
    public_key: [u8; 33],
) -> anyhow::Result<cosign2::Header> {
    Ok(fw_utils::hash::verify_cosign2_mem_with_third_party_keys(
        &CryptoApi::default(),
        bytes,
        &[public_key],
        true,
    )?)
}
