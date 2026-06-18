// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Read;

use super::read_cosign2_header_from_reader;
use crate::FileSystem;

pub(super) fn app_binary_size(elf_path: &str) -> anyhow::Result<u64> {
    Ok(FileSystem::default().metadata(elf_path, fs::Location::System)?.size)
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
