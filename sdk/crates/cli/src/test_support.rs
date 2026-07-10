// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

//! Shared helpers for the crate's unit tests.

use std::path::Path;

use tempfile::TempDir;

pub fn make_temp_dir(label: &str) -> TempDir {
    tempfile::Builder::new().prefix(&format!("foundation-test-{label}-")).tempdir().unwrap()
}

#[cfg(unix)]
pub fn link_fake_bin(bin: &Path) {
    std::os::unix::fs::symlink(Path::new(env!("FOUNDATION_FAKE_BIN")), bin).unwrap();
}

#[cfg(not(unix))]
pub fn link_fake_bin(bin: &Path) { std::fs::create_dir_all(bin).unwrap(); }
