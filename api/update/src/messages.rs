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
    pub patches: Vec<PatchProgress>,
    pub firmware_copy: FirmwareCopyProgress,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct FirmwareCopyProgress {
    pub copied_bytes: u64,
    pub total_bytes: u64,
}

/// Progress information for a single patch/release file
#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct PatchProgress {
    /// Size of the patch file in bytes
    pub file_size: u64,
    pub total_actions: u32,
    pub completed_actions: u32,
    /// Whether this patch requires a reboot after application
    pub requires_reboot: bool,
}

impl InstallProgress {
    const COPY_BYTES_PER_SECOND: f64 = 7.0 * 1024.0 * 1024.0;
    const PATCH_OVERHEAD_SECONDS: f64 = 2.0;
    const SECONDS_PER_ACTION: f64 = 1.5;
    const SECONDS_PER_MB: f64 = 5.0;

    pub fn action_completed(&mut self) {
        if let Some(patch) = self.patches.iter_mut().find(|p| p.completed_actions < p.total_actions) {
            patch.completed_actions += 1;
        }
    }

    pub fn set_firmware_copy(&mut self, progress: FirmwareCopyProgress) { self.firmware_copy = progress; }

    pub fn estimate_time_remaining_secs(&self) -> u64 {
        let total = self.time_total_secs();
        let completed = self.time_completed_secs();
        total.saturating_sub(completed)
    }

    pub fn completion_percentage(&self) -> u32 {
        let total = self.time_total_secs();
        if total == 0 {
            return 100;
        }

        let completed = self.time_completed_secs();
        ((completed as f64 / total as f64) * 100.0).min(99.0) as u32
    }

    pub fn time_total_secs(&self) -> u64 {
        let mut total = 0.0;

        let copy_time = self.firmware_copy.total_bytes as f64 / Self::COPY_BYTES_PER_SECOND;
        total += copy_time;

        for (idx, patch) in self.patches.iter().enumerate() {
            total += Self::PATCH_OVERHEAD_SECONDS;
            let mb = patch.file_size as f64 / (1024.0 * 1024.0);
            total += mb * Self::SECONDS_PER_MB;
            total += patch.total_actions as f64 * Self::SECONDS_PER_ACTION;

            if patch.requires_reboot && idx < self.patches.len() - 1 {
                total += copy_time;
            }
        }

        total as u64
    }

    pub fn time_completed_secs(&self) -> u64 {
        let mut completed = 0.0;

        let copy_time = self.firmware_copy.copied_bytes as f64 / Self::COPY_BYTES_PER_SECOND;
        completed += copy_time;

        for (idx, patch) in self.patches.iter().enumerate() {
            let mb = patch.file_size as f64 / (1024.0 * 1024.0);
            let file_work = mb * Self::SECONDS_PER_MB;
            let action_work = patch.total_actions as f64 * Self::SECONDS_PER_ACTION;

            if patch.completed_actions >= patch.total_actions {
                completed += Self::PATCH_OVERHEAD_SECONDS + file_work + action_work;
            } else if patch.completed_actions > 0 {
                completed += Self::PATCH_OVERHEAD_SECONDS + file_work;
                completed += patch.completed_actions as f64 * Self::SECONDS_PER_ACTION;
            }

            if patch.requires_reboot && idx < self.patches.len() - 1 {
                if patch.completed_actions >= patch.total_actions {
                    let reboot_copy_time =
                        self.firmware_copy.total_bytes as f64 / Self::COPY_BYTES_PER_SECOND;
                    completed += reboot_copy_time;
                }
            }
        }

        completed as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_completed() {
        let mut progress = InstallProgress {
            patches: vec![PatchProgress {
                file_size: 1024,
                total_actions: 5,
                completed_actions: 2,
                requires_reboot: false,
            }],
            firmware_copy: FirmwareCopyProgress { copied_bytes: 0, total_bytes: 0 },
        };

        progress.action_completed();
        assert_eq!(progress.patches[0].completed_actions, 3);

        progress.action_completed();
        assert_eq!(progress.patches[0].completed_actions, 4);
    }

    #[test]
    fn estimate_time_remaining() {
        let progress = InstallProgress {
            patches: vec![
                PatchProgress {
                    file_size: 5_242_880,
                    total_actions: 10,
                    completed_actions: 5,
                    requires_reboot: false,
                },
                PatchProgress {
                    file_size: 1_048_576,
                    total_actions: 5,
                    completed_actions: 0,
                    requires_reboot: true,
                },
                PatchProgress {
                    file_size: 10_485_760,
                    total_actions: 8,
                    completed_actions: 0,
                    requires_reboot: false,
                },
            ],
            firmware_copy: FirmwareCopyProgress { copied_bytes: 10_485_760, total_bytes: 10_485_760 },
        };

        let completed = progress.time_completed_secs();
        let total = progress.time_total_secs();
        let remaining = progress.estimate_time_remaining_secs();

        assert!(total > 0);
        assert!(completed > 0);
        assert_eq!(remaining, total.saturating_sub(completed));
    }

    #[test]
    fn completion_percentage() {
        let progress = InstallProgress {
            patches: vec![PatchProgress {
                file_size: 1_048_576,
                total_actions: 10,
                completed_actions: 5,
                requires_reboot: false,
            }],
            firmware_copy: FirmwareCopyProgress { copied_bytes: 10_485_760, total_bytes: 10_485_760 },
        };

        let percentage = progress.completion_percentage();
        assert!(percentage > 0 && percentage < 100);
    }

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
