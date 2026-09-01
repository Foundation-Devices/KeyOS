// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(ProgressUpdate)]
pub struct SubscribeUpdateProgress;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct StartUpdate {
    pub release_paths: Vec<String>,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ContinueUpdate;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<String, crate::Error>)]
pub struct FirmwareVersion;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ApplyDownloadedUpdate;

#[derive(Debug, server::Message)]
#[response(bool)]
pub struct GetUpdateApplied;

#[derive(Debug, server::Message)]
pub struct ClearUpdateApplied;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(UpdateStatus)]
pub struct GetUpdateStatus;

/// Status of the update system, used to determine if an update can be applied.
#[derive(Clone, Debug, Default, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct UpdateStatus {
    /// Whether there is a downloaded update ready to apply.
    pub downloaded_update: bool,
    /// Whether there is an update that was interrupted by reboot and needs to continue.
    pub needs_continue: bool,
    /// Whether a firmware update is currently being installed.
    pub installing: bool,
    /// Whether the battery level is sufficient for an update.
    pub sufficient_battery: bool,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum ProgressUpdate {
    DownloadProgress(DownloadProgress),
    // firmware files have been downloaded and are ready to apply
    DownloadComplete,
    // install progress
    InstallProgress(InstallProgress),
    // need reboot mid-update
    Rebooting,
    // completed update, and is about to reboot
    Done,
    InstallError(crate::Error),
    DownloadError(crate::DownloadError),
}

#[derive(Debug, Clone, Copy, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DownloadProgress {
    pub patches_total: u32,
    pub patches_complete: u32,
    pub chunks_received: u32,
    pub total_chunks: u32,
}

impl DownloadProgress {
    pub fn is_start(&self) -> bool { self.patches_complete == 0 && self.chunks_received == 0 }

    pub fn completion_percentage(&self) -> u32 {
        if self.total_chunks == 0 {
            return 0;
        }

        self.chunks_received.saturating_mul(100).saturating_div(self.total_chunks).min(100)
    }
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct InstallProgress {
    pub completion_percentage: u32,
    pub estimated_seconds_remaining: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_completion_percentage() {
        let progress = DownloadProgress {
            patches_total: 2,
            patches_complete: 0,
            chunks_received: 34,
            total_chunks: 100,
        };

        assert_eq!(progress.completion_percentage(), 34);
    }

    #[test]
    fn download_completion_percentage_handles_zero_total() {
        let progress =
            DownloadProgress { patches_total: 2, patches_complete: 0, chunks_received: 0, total_chunks: 0 };

        assert_eq!(progress.completion_percentage(), 0);
    }
}
