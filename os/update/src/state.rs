// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use file_backed::JsonBacked;
use fs::adapter::FsAdapter;
use fs::messages::{GetMetadata, Remove};
use serde::{Deserialize, Serialize};
use server::MessageAllowed;

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
    /// Expected firmware version when resuming a multi-stage update after reboot.
    /// If the running version differs (e.g. the user recovered to a different version
    /// from the bootloader), the pending update chain is cancelled.
    #[serde(default)]
    pub expected_version_on_resume: Option<String>,
    /// Whether the staged firmware still needs to be swapped into place before resuming.
    #[serde(default)]
    pub finalize_update_pending: bool,
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

#[derive(Debug)]
pub(crate) struct ConsumeUpdateStateInvalidatedMarkerError {
    pub(crate) marker_observed: bool,
    pub(crate) error: fs::Error,
}

pub(crate) fn consume_update_state_invalidated_marker<F>(
    fs: &F,
    clear_update_state: impl FnOnce() -> Result<(), fs::Error>,
) -> Result<bool, ConsumeUpdateStateInvalidatedMarkerError>
where
    F: FsAdapter,
    F::Permissions: MessageAllowed<GetMetadata> + MessageAllowed<Remove>,
{
    let marker_path = keyos::recovery::UPDATE_STATE_INVALIDATED_MARKER_PATH;
    match fs.metadata(marker_path, fs::Location::System) {
        Ok(_) => {}
        Err(fs::Error::FileNotFound) => return Ok(false),
        Err(error) => {
            return Err(ConsumeUpdateStateInvalidatedMarkerError { marker_observed: false, error });
        }
    }

    clear_update_state()
        .map_err(|error| ConsumeUpdateStateInvalidatedMarkerError { marker_observed: true, error })?;
    fs.remove(marker_path, fs::Location::System)
        .map_err(|error| ConsumeUpdateStateInvalidatedMarkerError { marker_observed: true, error })?;
    match fs.remove_if_exists(&crate::downloader::patches_dir(), fs::Location::System) {
        Ok(()) => {}
        Err(e) => log::warn!("failed to remove downloaded update patches after Recovery: {e:?}"),
    }

    Ok(true)
}
