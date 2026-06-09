// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use file_backed::JsonBacked;
use serde::{Deserialize, Serialize};

use crate::fs_permissions::FileSystemPermissions;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct UpdateState {
    /// Files downloaded and ready to apply
    pub downloaded: Option<DownloadedUpdate>,
    /// Remaining files to apply (for resuming after reboot)
    pub pending_apply: Vec<String>,
    /// Whether an update was applied and requires acknowledgment after reboot
    #[serde(default)]
    pub update_applied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadedUpdate {
    pub paths: Vec<String>,
}

impl UpdateState {
    pub fn load() -> JsonBacked<Self, FileSystemPermissions> {
        JsonBacked::new("update_state.json", fs::Location::SystemAppData).0
    }
}
