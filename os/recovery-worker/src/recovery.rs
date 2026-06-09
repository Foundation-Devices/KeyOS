// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{bail, Context};
use constant_time_eq::constant_time_eq;
use fs::Location;
use serde::{Deserialize, Serialize};
use server::{
    ArchiveRequest, BlockingArchive, BlockingArchiveAsyncHandler, BlockingArchiveHandler,
    ScalarEventSubscriber, ScalarEventSubscriptionHandler, ScalarHandler, ServerContext,
};
use xous::{DropDeallocate, PID};

use crate::utils::convert_cosign2_header;
use crate::{
    CopyArchive, GetAppBinVerificationState, GetArchiveState, GetLastError, Progress, ProgressKind,
    ReadArchive, ReadArchiveError, RecoveryWorkerServer, StartRecovery, SubscribeProgress,
    DOWNGRADE_NOT_ALLOWED_MSG,
};

power_manager::use_api!();
security::use_api!();

const KEYOS_RECOVERY_OLD_FILE_PATH: &str = "keyos/app.old";
const KEYOS_RECOVERY_FILE_PATH: &str = "keyos/app.new";

const TEMP_ARCHIVE_PATH: &str = "keyos/recovtmp.tar";
const TEMP_ARCHIVE_LOCATION: Location = Location::System;

/// Temporary file path for OS binary verification
const TEMP_OS_BINARY_PATH: &str = "keyos/app.tmp";
/// Temporary file path for bootloader verification
const TEMP_BOOTLOADER_PATH: &str = "boot.tmp";

const REBOOT_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub(crate) struct FileEntry {
    name: String,
    #[serde(with = "hex::serde")]
    hash: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub(crate) struct RecoveryManifest {
    version: String,
    files: Vec<FileEntry>,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub(crate) struct ValidArchive {
    pub(crate) path: String,
    pub(crate) location: Location,
    pub(crate) version: String,
    pub(crate) apps: Vec<String>,
    pub(crate) assets: Vec<(String, fs::Location)>,
    pub(crate) os_binary: String,
    pub(crate) bootloader: Option<String>,
    pub(crate) manifest: RecoveryManifest,
}

#[derive(Debug, Default, Clone)]
pub(crate) enum ArchiveState {
    #[default]
    None,
    Error(ReadArchiveError),
    ValidArchive(ValidArchive),
    CopiedArchive(ValidArchive),
}

#[derive(Debug, Default)]
pub(crate) enum RecoveryState {
    #[default]
    None,
    Valid {
        hash: String,
        version: String,
        build_date: String,
        fw_file_hash: [u8; 32],
        temp_file_path: String,
        temp_file_location: Location,
        fw_file_total_size: usize,
        timestamp: u32,
        is_pre_release: bool,
    },
    Copied {
        fw_file_hash: [u8; 32],
        timestamp: u32,
        is_pre_release: bool,
    },
    Invalid(String),
}

impl BlockingArchiveAsyncHandler<ReadArchive> for RecoveryWorkerServer {
    fn handle(
        &mut self,
        request: ArchiveRequest<ReadArchive>,
        _context: &mut ServerContext<Self>,
    ) -> <ReadArchive as BlockingArchive>::Response {
        let ArchiveRequest { message, response } = request;
        let ReadArchive { path, location } = message;
        response.respond(()).ok(); // Respond immediately to unblock the caller

        self.archive_state = match self.tar_verify(&path, location) {
            Ok(res) => {
                let progress = Progress {
                    kind: ProgressKind::ArchiveRead,
                    is_completed: true,
                    is_error: false,
                    progress: 1.0,
                };
                if let Some(subscriber) = &self.progress_subscriber {
                    subscriber.send(&progress).ok();
                }

                ArchiveState::ValidArchive(res)
            }
            Err(e) => {
                log::error!("Failed to verify tar archive: {path} @ {location:?}");

                let progress = Progress {
                    kind: ProgressKind::ArchiveRead,
                    is_completed: true,
                    is_error: true,
                    progress: 1.0,
                };
                if let Some(subscriber) = &self.progress_subscriber {
                    subscriber.send(&progress).ok();
                }

                ArchiveState::Error(ReadArchiveError::InternalError(e.to_string()))
            }
        };
    }

    fn default_response() -> <ReadArchive as server::BlockingArchive>::Response {}
}

impl BlockingArchiveHandler<GetArchiveState> for RecoveryWorkerServer {
    fn handle(
        &mut self,
        _msg: GetArchiveState,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetArchiveState as BlockingArchive>::Response {
        log::debug!("GetArchiveState request received, returning current archive state");
        self.archive_state.clone().into()
    }
}

impl ScalarEventSubscriptionHandler<SubscribeProgress> for RecoveryWorkerServer {
    fn handle(
        &mut self,
        _msg: SubscribeProgress,
        subscriber: ScalarEventSubscriber<Progress>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        log::info!("Received SubscribeSystemInfo message, adding subscriber {:?}", subscriber);
        self.progress_subscriber = Some(Rc::new(subscriber));

        Ok(())
    }
}

impl ScalarHandler<CopyArchive> for RecoveryWorkerServer {
    fn handle(&mut self, _msg: CopyArchive, _sender: PID, _context: &mut ServerContext<Self>) {
        self.last_error = None;
        let ArchiveState::ValidArchive(ValidArchive {
            path,
            location,
            version,
            apps,
            assets,
            os_binary,
            bootloader,
            manifest,
        }) = &self.archive_state
        else {
            log::error!("No valid archive selected");
            return;
        };

        let os_binary = os_binary.clone();
        let bootloader = bootloader.clone();
        self.os_binary_state = RecoveryState::None;

        self.fs.remove(TEMP_ARCHIVE_PATH, TEMP_ARCHIVE_LOCATION).ok();
        let is_ok = fw_utils::hash::copy_file_progress(
            &self.fs,
            path,
            *location,
            TEMP_ARCHIVE_PATH,
            TEMP_ARCHIVE_LOCATION,
            |progress| {
                let progress = Progress {
                    kind: ProgressKind::ArchiveCopy,
                    is_completed: false,
                    is_error: false,
                    progress,
                };
                if let Some(subscriber) = &self.progress_subscriber {
                    subscriber.send(&progress).ok();
                }
            },
        )
        .is_ok();

        let progress =
            Progress { kind: ProgressKind::ArchiveCopy, is_completed: true, is_error: !is_ok, progress: 1.0 };
        if let Some(subscriber) = &self.progress_subscriber {
            subscriber.send(&progress).ok();
        }

        // Verify the KeyOS image after copying the archive
        if is_ok {
            self.archive_state = ArchiveState::CopiedArchive(ValidArchive {
                path: TEMP_ARCHIVE_PATH.to_string(),
                location: TEMP_ARCHIVE_LOCATION,
                version: version.clone(),
                apps: apps.clone(),
                assets: assets.clone(),
                os_binary: os_binary.clone(),
                bootloader: bootloader.clone(),
                manifest: manifest.clone(),
            });

            if self
                .verify_binary(false, &os_binary)
                .inspect_err(|e| log::error!("{os_binary} verification error: {e:?}"))
                .is_err()
            {
                let progress = Progress {
                    kind: ProgressKind::AppBinVerify,
                    is_completed: true,
                    is_error: true,
                    progress: 1.0,
                };
                if let Some(subscriber) = &self.progress_subscriber {
                    subscriber.send(&progress).ok();
                }
            }

            if let Some(bootloader) = &bootloader {
                if self
                    .verify_binary(true, bootloader)
                    .inspect_err(|e| log::error!("{bootloader} verification error: {e:?}"))
                    .is_err()
                {
                    let progress = Progress {
                        kind: ProgressKind::AppBinVerify,
                        is_completed: true,
                        is_error: true,
                        progress: 1.0,
                    };
                    if let Some(subscriber) = &self.progress_subscriber {
                        subscriber.send(&progress).ok();
                    }
                }
            }
        }
    }
}

impl ScalarHandler<StartRecovery> for RecoveryWorkerServer {
    fn handle(&mut self, _msg: StartRecovery, _sender: PID, _context: &mut ServerContext<Self>) {
        if !self.start_recovery() {
            if let Some(subscriber) = &self.progress_subscriber {
                subscriber
                    .send(&Progress {
                        kind: ProgressKind::Extracting,
                        is_completed: true,
                        is_error: true,
                        progress: 1.0,
                    })
                    .ok();
            }
        } else {
            self.delete_archive().ok();

            if let Some(subscriber) = &self.progress_subscriber {
                subscriber
                    .send(&Progress {
                        kind: ProgressKind::RebootCountdown,
                        is_completed: false,
                        is_error: false,
                        progress: 1.0,
                    })
                    .ok();
            }

            std::thread::sleep(REBOOT_DELAY);
            PowerManagerApi::default().reboot();
        }
    }
}

impl BlockingArchiveHandler<GetAppBinVerificationState> for RecoveryWorkerServer {
    fn handle(
        &mut self,
        _msg: GetAppBinVerificationState,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetAppBinVerificationState as BlockingArchive>::Response {
        (&self.os_binary_state).into()
    }
}

impl BlockingArchiveHandler<GetLastError> for RecoveryWorkerServer {
    fn handle(
        &mut self,
        _msg: GetLastError,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Option<String> {
        self.last_error.clone()
    }
}

impl RecoveryWorkerServer {
    fn start_recovery(&mut self) -> bool {
        self.last_error = None;

        let ArchiveState::CopiedArchive(ValidArchive { apps, assets, os_binary, bootloader, .. }) =
            &self.archive_state
        else {
            self.last_error = Some("No valid archive selected".to_string());
            return false;
        };
        let os_binary = os_binary.clone();
        let bootloader = bootloader.clone();

        let num_steps_copy_os_binary = 15;
        let num_steps_read_app_bin = 10;
        let num_steps_finalize_os_binary = 10;
        let num_steps_copy_bootloader = 10;
        let num_steps_finalize_bootloader = 10;
        let num_steps_copy_apps = apps.len();
        let num_steps_copy_assets = assets.len();

        let num_steps_total = num_steps_copy_os_binary
            + num_steps_read_app_bin
            + num_steps_finalize_os_binary
            + if bootloader.is_some() {
                num_steps_copy_bootloader + num_steps_finalize_bootloader
            } else {
                0
            }
            + num_steps_copy_apps
            + num_steps_copy_assets;
        let mut num_steps_done = 0;

        log::info!("Copying {os_binary}");
        let subscriber = self.progress_subscriber.clone();

        if let Err(e) = self.copy_binary(false, &os_binary, |progress| {
            let Some(subscriber) = &subscriber else { return };
            subscriber
                .send(&Progress {
                    kind: ProgressKind::Extracting,
                    is_completed: false,
                    is_error: false,
                    progress: (num_steps_done as f32 + progress * num_steps_copy_os_binary as f32)
                        / (num_steps_total as f32),
                })
                .ok();
        }) {
            log::error!("Failed to copy {os_binary}: {e:?}");
            self.last_error = Some(format!("Failed to copy {os_binary}: {e:?}"));
            return false;
        }
        num_steps_done += num_steps_copy_os_binary;

        log::info!("Reading back {}", &os_binary);
        if let Err(e) = self.readback_os_binary(|progress| {
            let Some(subscriber) = &subscriber else { return };
            subscriber
                .send(&Progress {
                    kind: ProgressKind::Extracting,
                    is_completed: false,
                    is_error: false,
                    progress: (num_steps_done as f32 + progress * num_steps_read_app_bin as f32)
                        / (num_steps_total as f32),
                })
                .ok();
        }) {
            log::error!("Failed to readback os binary: {e:?}");
            self.last_error = Some(format!("Failed to readback os binary: {e:?}"));
            return false;
        }
        num_steps_done += num_steps_read_app_bin;

        log::info!("Finalizing {}", os_binary.clone());
        if let Err(e) = self.finalize_os_binary(false, &os_binary, |progress| {
            let Some(subscriber) = &subscriber else { return };
            subscriber
                .send(&Progress {
                    kind: ProgressKind::Extracting,
                    is_completed: false,
                    is_error: false,
                    progress: (num_steps_done as f32 + progress * num_steps_finalize_os_binary as f32)
                        / (num_steps_total as f32),
                })
                .ok();
        }) {
            log::error!("Failed to finalize os binary: {e:?}");
            self.last_error = Some(format!("Failed to finalize os binary: {e:?}"));
            return false;
        }
        num_steps_done += num_steps_finalize_os_binary;

        if let Some(bootloader) = bootloader {
            if let Err(e) = self.copy_binary(true, &bootloader, |progress| {
                let Some(subscriber) = &subscriber else { return };
                subscriber
                    .send(&Progress {
                        kind: ProgressKind::Extracting,
                        is_completed: false,
                        is_error: false,
                        progress: (num_steps_done as f32 + progress * num_steps_copy_bootloader as f32)
                            / (num_steps_total as f32),
                    })
                    .ok();
            }) {
                log::error!("Failed to copy bootloader ({bootloader}): {e:?}");
                self.last_error = Some(format!("Failed to copy bootloader ({bootloader}): {e:?}"));
                return false;
            }
            num_steps_done += num_steps_copy_bootloader;

            if let Err(e) = self.finalize_os_binary(true, &bootloader, |progress| {
                let Some(subscriber) = &subscriber else { return };
                subscriber
                    .send(&Progress {
                        kind: ProgressKind::Extracting,
                        is_completed: false,
                        is_error: false,
                        progress: (num_steps_done as f32 + progress * num_steps_finalize_bootloader as f32)
                            / (num_steps_total as f32),
                    })
                    .ok();
            }) {
                log::error!("Failed to finalize bootloader: ({bootloader}): {e:?}");
                self.last_error = Some(format!("Failed to finalize bootloader: ({bootloader}): {e:?}"));
                return false;
            }
            num_steps_done += num_steps_finalize_bootloader;
        }

        log::info!("Wiping target directories");
        self.wipe_recovery_targets();

        log::info!("Copying apps");
        if let Err(e) = self.copy_apps(|progress| {
            let Some(subscriber) = &subscriber else { return };
            subscriber
                .send(&Progress {
                    kind: ProgressKind::Extracting,
                    is_completed: false,
                    is_error: false,
                    progress: (num_steps_done as f32 + progress * num_steps_copy_apps as f32)
                        / (num_steps_total as f32),
                })
                .ok();
        }) {
            log::error!("Failed to copy and validate apps: {e:?}");
            self.last_error = Some(format!("Failed to copy and validate apps: {e:?}"));
            return false;
        }
        num_steps_done += num_steps_copy_apps;

        log::info!("Copying assets");
        if let Err(e) = self.copy_assets(|progress| {
            let Some(subscriber) = &subscriber else { return };
            subscriber
                .send(&Progress {
                    kind: ProgressKind::Extracting,
                    is_completed: false,
                    is_error: false,
                    progress: (num_steps_done as f32 + progress * num_steps_copy_assets as f32)
                        / (num_steps_total as f32),
                })
                .ok();
        }) {
            log::error!("Failed to copy and validate assets: {e:?}");
            self.last_error = Some(format!("Failed to copy and validate assets: {e:?}"));
            return false;
        }

        true
    }

    fn copy_binary(
        &mut self,
        is_bootloader: bool,
        binary: &str,
        progress_fn: impl Fn(f32),
    ) -> anyhow::Result<()> {
        let state = if is_bootloader { &self.bootloader_state } else { &self.os_binary_state };
        let RecoveryState::Valid {
            temp_file_path,
            temp_file_location,
            fw_file_total_size,
            fw_file_hash,
            is_pre_release,
            timestamp,
            ..
        } = state
        else {
            bail!("No valid recovery state");
        };

        let (_old_name, new_name, target_location) = os_binary_to_file_name_and_location(binary)?;
        self.fs.remove(&new_name, target_location).ok();

        if is_bootloader {
            // Strip the cosign2 header as it's not supported for the bootloader
            let header_size = cosign2::Header::DEFAULT_SIZE;
            let new_size = fw_file_total_size.saturating_sub(header_size);

            self.copy_file_with_offset(
                temp_file_path,
                *temp_file_location,
                &new_name,
                target_location,
                header_size,
                new_size,
                progress_fn,
            )?;
        } else {
            // Copy the entire file as-is
            fw_utils::hash::copy_file_progress(
                &self.fs,
                temp_file_path,
                *temp_file_location,
                &new_name,
                target_location,
                progress_fn,
            )?;
        }

        // Clean up the temp file after a successful copy
        self.fs.remove(temp_file_path, *temp_file_location).ok();

        if is_bootloader {
            self.bootloader_state = RecoveryState::Copied {
                fw_file_hash: *fw_file_hash,
                is_pre_release: *is_pre_release,
                timestamp: *timestamp,
            };
        } else {
            self.os_binary_state = RecoveryState::Copied {
                fw_file_hash: *fw_file_hash,
                is_pre_release: *is_pre_release,
                timestamp: *timestamp,
            };
        }

        Ok(())
    }

    /// Copy a file with an offset (skipping the first `offset` bytes).
    /// Used for stripping cosign2 headers from bootloader files.
    fn copy_file_with_offset(
        &self,
        src_path: &str,
        src_location: Location,
        dst_path: &str,
        dst_location: Location,
        offset: usize,
        size: usize,
        progress_fn: impl Fn(f32),
    ) -> anyhow::Result<()> {
        use std::io::{Seek, SeekFrom};

        let mut src_file = self.fs.open_file(
            src_path,
            src_location,
            fs::OpenFlags { read: true, write: false, create: false },
        )?;

        let mut dst_file = self.fs.open_file(
            dst_path,
            dst_location,
            fs::OpenFlags { read: false, write: true, create: true },
        )?;

        // Skip the header
        src_file.seek(SeekFrom::Start(offset as u64))?;

        const CHUNK_SIZE: usize = 32 * 1024;
        let mut bytes_copied = 0;

        progress_fn(0.0);

        while bytes_copied < size {
            let bytes_remaining = size - bytes_copied;
            let chunk_size = bytes_remaining.min(CHUNK_SIZE);

            let written = src_file.copy_block_to(&mut dst_file, chunk_size)?;
            if written == 0 {
                break;
            }

            bytes_copied += written;
            progress_fn(bytes_copied as f32 / size as f32);
        }

        // Truncate to remove any leftover bytes if dst file was larger
        dst_file.truncate()?;
        progress_fn(1.0);

        Ok(())
    }

    fn readback_os_binary(&mut self, progress_fn: impl Fn(f32)) -> anyhow::Result<()> {
        let ArchiveState::CopiedArchive(ValidArchive { version, os_binary, manifest, .. }) =
            &self.archive_state
        else {
            bail!("No copied archive");
        };
        let (_old_name, new_name, location) = os_binary_to_file_name_and_location(os_binary)?;

        let RecoveryState::Copied { fw_file_hash, .. } = &self.os_binary_state else {
            bail!("No valid OS binary state to readback");
        };

        let header = fw_utils::hash::verify_cosign2(
            &self.fs,
            &self.crypto,
            new_name,
            location,
            progress_fn,
            cfg!(feature = "production"),
        )
        .context("couldn't verify cosign2 header")?;
        if !constant_time_eq(header.binary_hash(), fw_file_hash) {
            bail!("{os_binary} hash mismatch");
        }

        let os_binary_path_tar = if os_binary == "app.bin" {
            format!("{version}/keyos/{os_binary}")
        } else {
            format!("{version}/{os_binary}")
        };

        if !verify_manifest_hash(manifest, &os_binary_path_tar, *fw_file_hash) {
            bail!("{os_binary} hash mismatch");
        }

        Ok(())
    }

    fn finalize_os_binary(
        &mut self,
        is_bootloader: bool,
        binary: &str,
        progress_fn: impl Fn(f32),
    ) -> anyhow::Result<()> {
        let state = if is_bootloader { &self.bootloader_state } else { &self.os_binary_state };
        let RecoveryState::Copied { is_pre_release, timestamp, .. } = state else {
            bail!("No valid binary state to finalize for {binary}");
        };
        let (old_name, new_name, location) = os_binary_to_file_name_and_location(binary)?;
        let final_name = if binary == "app.bin" { "keyos/app.bin" } else { binary };

        progress_fn(0.0);

        // .bin -> .old
        // .new -> .bin
        log::debug!("Removing old file: {old_name} -> {new_name} (location: {location:?})");
        self.fs
            .remove(&old_name, location)
            .inspect_err(|e| {
                log::error!("Removing old file: {e:?}");
            })
            .ok();
        progress_fn(0.1);

        log::debug!("Renaming {final_name} to {old_name} @ {location:?}");
        self.fs
            .rename(final_name, &old_name, location)
            .inspect_err(|e| {
                log::error!("Renaming {final_name} to {old_name}: {e:?}");
            })
            .ok();
        progress_fn(0.4);

        log::debug!("Renaming {new_name} to {final_name} @ {location:?}");
        let rename_ok = self
            .fs
            .rename(&new_name, final_name, location)
            .inspect_err(|e| {
                log::error!("Unable to rename {new_name} to {final_name}: {e:?}");
            })
            .is_ok();
        progress_fn(0.6);

        if !rename_ok {
            log::warn!("Couldn't rename, reverting by renaming {old_name} to {final_name} @ {location:?}");
            self.fs
                .rename(&old_name, final_name, location)
                .inspect_err(|e| {
                    log::error!("Unable to rename {old_name} to {final_name} (rollback): {e:?}");
                })
                .ok();
        }

        let timestamp_update_ok = if !is_pre_release {
            if is_bootloader {
                true
            } else {
                Security::default()
                    .set_firmware_timestamp((*timestamp).into())
                    .inspect_err(|e| {
                        log::error!("Unable to set firmware timestamp: {e:?}");
                    })
                    .is_ok()
            }
        } else {
            log::warn!("Skipping firmware timestamp update for a pre-release / recovery firmware");
            rename_ok
        };

        if !rename_ok {
            bail!("unable to rename {binary}");
        }

        if !timestamp_update_ok {
            bail!("unable to set firmware timestamp");
        }

        progress_fn(1.0);

        Ok(())
    }

    fn delete_archive(&mut self) -> anyhow::Result<()> {
        if let ArchiveState::CopiedArchive(ValidArchive { path, location, .. }) = &self.archive_state {
            self.fs.remove(path, *location).ok();
            self.archive_state = ArchiveState::None;
        } else {
            bail!("No copied archive to delete");
        }

        Ok(())
    }

    fn verify_binary(&mut self, is_bootloader: bool, binary: &str) -> anyhow::Result<()> {
        let ArchiveState::CopiedArchive(ValidArchive {
            path, location, version: tar_version, manifest, ..
        }) = &self.archive_state
        else {
            bail!("No copied archive");
        };

        let _ = os_binary_to_file_name_and_location(binary)?;

        if is_bootloader {
            self.bootloader_state = RecoveryState::None;
        } else {
            self.os_binary_state = RecoveryState::None;
        }

        let security = Security::default();

        let timestamp = if is_bootloader {
            security.bootloader_build_date()
        } else {
            security.firmware_timestamp().map(|timestamp| Some(Into::<u32>::into(timestamp) as _))
        };

        let Ok(Some(last_fw_timestamp)) = timestamp else {
            bail!("unable to get last firmware timestamp");
        };

        let binary_path_tar = if binary == "app.bin" {
            format!("{tar_version}/keyos/{binary}")
        } else {
            format!("{tar_version}/{binary}")
        };

        let (temp_file_path, temp_file_location) = if is_bootloader {
            (TEMP_BOOTLOADER_PATH.to_string(), Location::Boot)
        } else {
            (TEMP_OS_BINARY_PATH.to_string(), Location::System)
        };
        self.fs.remove(&temp_file_path, temp_file_location).ok();

        let fw_file_total_size = match self.tar_extract_file(
            path,
            *location,
            &binary_path_tar,
            &temp_file_path,
            temp_file_location,
            |progress| {
                let progress = Progress {
                    kind: ProgressKind::AppBinVerify,
                    is_completed: false,
                    is_error: false,
                    progress: progress * 0.5, // The first 50% is extraction
                };
                if let Some(subscriber) = &self.progress_subscriber {
                    subscriber.send(&progress).ok();
                }
            },
        ) {
            Ok(size) => size,
            Err(e) => {
                self.fs.remove(&temp_file_path, temp_file_location).ok();
                self.delete_archive()?;
                bail!("unable to extract {binary_path_tar} from the archive: {e}");
            }
        };

        let header = fw_utils::hash::verify_cosign2(
            &self.fs,
            &self.crypto,
            &temp_file_path,
            temp_file_location,
            |progress| {
                let progress = Progress {
                    kind: ProgressKind::AppBinVerify,
                    is_completed: false,
                    is_error: false,
                    progress: 0.5 + progress * 0.5, // The second 50% is verification
                };
                if let Some(subscriber) = &self.progress_subscriber {
                    subscriber.send(&progress).ok();
                }
            },
            cfg!(feature = "production"),
        );
        let err_str = format!("{:?}", header.as_ref().err());

        let (fw_file_hash, hash, version, build_date, timestamp, is_valid) = convert_cosign2_header(header);
        let is_valid = is_valid && verify_manifest_hash(manifest, &binary_path_tar, fw_file_hash);

        if is_valid && (timestamp as u64) < last_fw_timestamp {
            self.fs.remove(&temp_file_path, temp_file_location).ok();
            self.delete_archive()?;

            let downgrade_err = DOWNGRADE_NOT_ALLOWED_MSG.to_string();

            if is_bootloader {
                self.bootloader_state = RecoveryState::Invalid(downgrade_err.clone());
            } else {
                self.os_binary_state = RecoveryState::Invalid(downgrade_err.clone());
            }
            self.last_error = Some(downgrade_err.clone());
            self.archive_state = ArchiveState::Error(ReadArchiveError::DowngradeNotAllowed);

            let progress = Progress {
                kind: ProgressKind::AppBinVerify,
                is_completed: true,
                is_error: true,
                progress: 1.0,
            };
            if let Some(subscriber) = &self.progress_subscriber {
                subscriber.send(&progress).ok();
            }

            return Ok(());
        }

        if is_valid {
            let is_pre_release = version.contains("alpha") || version.contains("beta");

            let state = RecoveryState::Valid {
                hash,
                version,
                build_date,
                fw_file_hash,
                temp_file_path: temp_file_path.clone(),
                temp_file_location,
                fw_file_total_size,
                timestamp,
                is_pre_release,
            };

            if is_bootloader {
                self.bootloader_state = state;
            } else {
                self.os_binary_state = state;
            }
        } else {
            self.fs.remove(&temp_file_path, temp_file_location).ok();
            self.delete_archive()?;

            if is_bootloader {
                self.bootloader_state = RecoveryState::Invalid(err_str.clone());
                self.last_error = Some(err_str.clone());
                self.archive_state = ArchiveState::Error(ReadArchiveError::InternalError(err_str));
            } else {
                self.os_binary_state = RecoveryState::Invalid(err_str.clone());
                self.last_error = Some(err_str.clone());
                self.archive_state = ArchiveState::Error(ReadArchiveError::InternalError(err_str));
            }
        }

        let progress = Progress {
            kind: ProgressKind::AppBinVerify,
            is_completed: true,
            is_error: !is_valid,
            progress: 1.0,
        };
        if let Some(subscriber) = &self.progress_subscriber {
            subscriber.send(&progress).ok();
        }

        Ok(())
    }

    fn wipe_recovery_targets(&self) {
        let ArchiveState::CopiedArchive(ValidArchive { apps, assets, .. }) = &self.archive_state else {
            return;
        };

        // Wipe the entire apps directory so the on-disk app set exactly matches the archive
        // This also removes apps that were deleted between versions
        // If the archive carries no apps (e.g. an OS-only update), leave the directory alone
        if !apps.is_empty() {
            log::info!("Wiping apps directory: keyos/apps");
            self.fs.remove("keyos/apps", Location::System).ok();
        }

        // Wipe each asset root directory present in the archive. Using the root
        // (e.g. `keyos/common`, `blassets`, `common`) rather than individual file-parent
        // directories ensures sub-directories removed between versions are also cleaned up
        let mut asset_roots: HashSet<(String, Location)> = HashSet::new();
        for (tar_asset_path, asset_location) in assets {
            let fs_path = asset_tar_path_to_fs_path(tar_asset_path, *asset_location);
            if let Some(root) = asset_fs_path_to_wipe_root(&fs_path, *asset_location) {
                asset_roots.insert((root, *asset_location));
            }
        }

        for (root, location) in &asset_roots {
            log::info!("Wiping asset root: {root} @ {location:?}");
            self.fs.remove(root, *location).ok();
        }
    }

    fn copy_apps(&self, progress_fn: impl Fn(f32)) -> anyhow::Result<()> {
        let ArchiveState::CopiedArchive(ValidArchive { path, location, apps, manifest, .. }) =
            &self.archive_state
        else {
            bail!("No valid archive selected");
        };

        progress_fn(0.0);
        if apps.is_empty() {
            progress_fn(1.0);
            return Ok(());
        }

        for (i, tar_app_path) in apps.iter().enumerate() {
            let file_path_parts = tar_app_path.split('/').collect::<Vec<_>>();

            let Some(app_name) = file_path_parts.last() else {
                continue;
            };

            let tar_elf_path = format!("{tar_app_path}/app.elf");
            let (file_mem, file_size) =
                self.tar_read_file_progress(path, *location, &tar_elf_path, |_| ())?;

            let header = fw_utils::hash::verify_cosign2_mem(
                &self.crypto,
                &file_mem.as_slice::<u8>()[..file_size],
                cfg!(feature = "production"),
            );
            let (hash, _, _, _, _, is_valid) = convert_cosign2_header(header);
            let is_valid = is_valid && verify_manifest_hash(manifest, &tar_elf_path, hash);
            if !is_valid {
                bail!("App {tar_app_path} is invalid");
            }

            let fs_app_dir = format!("keyos/apps/{app_name}");
            self.create_dir_all(&fs_app_dir, Location::System);

            let fs_elf_path = &format!("{fs_app_dir}/app.elf");
            self.fs.remove(fs_elf_path, Location::System).ok();

            // Copy the app ELF file
            fw_utils::hash::write_file_progress(
                &self.fs,
                fs_elf_path,
                Location::System,
                &file_mem,
                file_size,
                |_| (),
            )?;

            // Read and verify the app ELF file
            let (file_mem, file_size) =
                fw_utils::hash::read_file_progress(&self.fs, fs_elf_path, Location::System, |_| ())
                    .context("couldn't read the app ELF file back")?;
            let header = fw_utils::hash::verify_cosign2_mem(
                &self.crypto,
                &file_mem.as_slice::<u8>()[..file_size],
                cfg!(feature = "production"),
            );
            let (hash, _, _, _, _, is_valid) = convert_cosign2_header(header);
            let is_valid = is_valid && verify_manifest_hash(manifest, &tar_elf_path, hash);
            if !is_valid {
                bail!("App readback from {fs_elf_path} failed");
            }

            // Read and copy the manifest file
            let tar_manifest_path = format!("{tar_app_path}/manifest.json");
            let (file_mem, file_size) =
                self.tar_read_file_progress(path, *location, &tar_manifest_path, |_| ())?;

            let fs_manifest_path = &format!("{fs_app_dir}/manifest.json");

            self.fs.remove(fs_manifest_path, Location::System).ok();
            fw_utils::hash::write_file_progress(
                &self.fs,
                fs_manifest_path,
                Location::System,
                &file_mem,
                file_size,
                |_| (),
            )?;

            // Read and verify the app manifest file
            let (mem, size) =
                fw_utils::hash::read_file_progress(&self.fs, fs_manifest_path, Location::System, |_| ())
                    .context("couldn't read the manifest file back")?;
            let manifest_hash = self
                .crypto
                .sha256(&mem.as_slice::<u8>()[..size])
                .context("couldn't calculate manifest file hash")?;
            if !verify_manifest_hash(manifest, &tar_manifest_path, manifest_hash) {
                bail!("App manifest {fs_manifest_path} hash mismatch");
            }

            progress_fn(((i + 1) as f32) / (apps.len() as f32));
        }

        progress_fn(1.0);

        Ok(())
    }

    fn copy_assets(&self, progress_fn: impl Fn(f32)) -> anyhow::Result<()> {
        let ArchiveState::CopiedArchive(ValidArchive { path, location, assets, manifest, .. }) =
            &self.archive_state
        else {
            bail!("No valid archive selected");
        };

        progress_fn(0.0);

        if assets.is_empty() {
            progress_fn(1.0);
            return Ok(());
        }

        for (i, (tar_asset_path, asset_location)) in assets.iter().enumerate() {
            let (file_mem, file_size) =
                self.tar_read_file_progress(path, *location, tar_asset_path, |_| ())?;

            // Remove the version from the path and apply location-specific fixes
            let fs_asset_path = asset_tar_path_to_fs_path(tar_asset_path, *asset_location);
            let mut asset_path_parts = fs_asset_path.split('/').collect::<Vec<_>>();
            self.fs.remove(&fs_asset_path, *asset_location).ok();

            asset_path_parts.pop();
            let fs_asset_dir = asset_path_parts.join("/");
            self.create_dir_all(&fs_asset_dir, *asset_location);

            // Copy the asset file
            log::debug!("Copying asset file: {tar_asset_path} -> {fs_asset_path} @ {asset_location:?}");
            fw_utils::hash::write_file_progress(
                &self.fs,
                &fs_asset_path,
                *asset_location,
                &file_mem,
                file_size,
                |_| (),
            )?;

            // Read and verify the asset file
            let (mem, size) =
                fw_utils::hash::read_file_progress(&self.fs, fs_asset_path, *asset_location, |_| ())
                    .context("couldn't read the asset file back")?;
            let asset_hash = self
                .crypto
                .sha256(&mem.as_slice::<u8>()[..size])
                .context("couldn't calculate asset file hash")?;
            if !verify_manifest_hash(manifest, tar_asset_path, asset_hash) {
                bail!("Asset {tar_asset_path} hash mismatch");
            }

            progress_fn(((i + 1) as f32) / (assets.len() as f32));
        }

        progress_fn(1.0);

        Ok(())
    }

    /// Checks that the tar file contains all the required files of the KeyOS recovery archive.
    fn tar_verify(&mut self, path: &str, location: Location) -> anyhow::Result<ValidArchive> {
        let file =
            self.fs.open_file(path, location, fs::OpenFlags { read: true, write: false, create: false })?;

        let mut tar_version = None;
        let mut apps = Vec::new();
        let mut assets = Vec::new();
        let mut bootloader = None;
        let mut manifest = None;
        let mut os_binary = None;

        let mut archive = tar::Archive::new(file);
        for entry in archive.entries_with_seek()? {
            let entry = entry?;
            if !entry.header().entry_type().is_file() {
                continue;
            }

            let file_path = entry.path()?.to_string_lossy().to_string();
            let path_parts = file_path.split('/').map(ToString::to_string).collect::<Vec<_>>();
            let file_name = path_parts.last().cloned().unwrap_or("".to_string());

            let version = path_parts.first();
            let is_app_dir = path_parts.get(1).map(|p| p == "keyos").unwrap_or_default()
                && path_parts.get(2).map(|p| p == "apps").unwrap_or_default();
            let is_app_resource =
                is_app_dir && path_parts.get(4).map(|p| p == "resources").unwrap_or_default();
            let is_asset = path_parts.get(1).map(|p| p == "keyos").unwrap_or_default()
                && path_parts.get(2).map(|p| p == "common").unwrap_or_default();
            let is_boot_asset =
                path_parts.get(1).map(|p| p == "blassets" || p == "common-boot").unwrap_or_default();
            let is_recovery = file_name == "recovery.bin";
            let is_keyos_image = file_name == "app.bin";
            let is_bootloader = file_name == "boot.bin" || file_name == "boot.cip";
            let is_manifest = path_parts.get(1).map(|p| p == "manifest.json").unwrap_or_default();

            let is_version_number =
                version.map(|v| v.chars().filter(|c| *c == '.').count() == 2).unwrap_or_default();
            if tar_version.is_none() && is_version_number {
                log::debug!("Found tar version: {}", version.as_ref().unwrap());
                tar_version = version.cloned();
            }

            if is_app_dir && file_name.ends_with(".elf") {
                let mut path_parts = path_parts.clone();
                path_parts.pop();
                let app_dir = path_parts.join("/");
                log::debug!("Found app dir: {app_dir}");
                apps.push(app_dir);
            } else if is_app_resource {
                log::debug!("Found app resource: {file_name} (path: {file_path:?})");
                assets.push((file_path, Location::System));
            } else if is_asset {
                log::debug!("Found common asset: {file_name} (path: {file_path:?})");
                assets.push((file_path, Location::System));
            } else if is_boot_asset {
                log::debug!("Found boot asset: {file_name} (path: {file_path:?})");
                assets.push((file_path, Location::Boot));
            } else if is_bootloader {
                log::debug!("Found bootloader: {file_name} (path: {file_path:?})");
                bootloader = Some(file_name.clone());
            } else if is_recovery {
                log::debug!("Found recovery OS: {file_name} (path: {file_path:?})");
                os_binary = Some(file_name.clone());
            } else if is_keyos_image {
                log::debug!("Found KeyOS image: {file_name} (path: {file_path:?})");
                os_binary = Some(file_name.clone());
            } else if is_manifest {
                log::debug!("Found manifest file: {file_name} (path: {file_path:?})");
                manifest = Some(self.verify_manifest_file(&tar_version, entry)?);
            }
        }

        let Some(manifest) = manifest else {
            log::error!("Missing required manifest file in the archive");
            self.archive_state = ArchiveState::Error(ReadArchiveError::MissingRequiredFiles);
            bail!("Missing required manifest file");
        };

        let Some(version) = tar_version else {
            log::error!("No version found in the tar archive");

            self.archive_state = ArchiveState::Error(ReadArchiveError::UnsupportedFormat);
            bail!("No version found in the tar archive");
        };

        let Some(os_binary) = os_binary else {
            log::error!("No app.bin or recovery.bin found in the archive");

            self.archive_state = ArchiveState::Error(ReadArchiveError::MissingRequiredFiles);
            bail!("No app.bin or recovery.bin found in the archive");
        };

        Ok(ValidArchive {
            path: path.to_string(),
            location,
            version,
            apps,
            assets,
            os_binary,
            bootloader,
            manifest,
        })
    }

    fn verify_manifest_file<R: std::io::Read>(
        &self,
        tar_version: &Option<String>,
        entry: tar::Entry<R>,
    ) -> anyhow::Result<RecoveryManifest> {
        let size = entry.size() as usize;
        if size < cosign2::Header::DEFAULT_SIZE {
            bail!("Manifest file is too small to contain a valid cosign2 header");
        }

        let (mem, _) =
            fw_utils::hash::read_progress(entry, size, |_| ()).context("read manifest file from tar")?;

        let manifest_bytes_without_cosign2 = &mem.as_slice::<u8>()[cosign2::Header::DEFAULT_SIZE..].to_vec();
        let json = std::ffi::CStr::from_bytes_until_nul(manifest_bytes_without_cosign2)?.to_bytes();

        let Ok(header) =
            fw_utils::hash::verify_cosign2_mem(&self.crypto, &mem.as_slice::<u8>()[..size], false)
        else {
            bail!("Manifest header is invalid")
        };
        // The recovery archive manifest is allowed to be single-signed for simplicity of the release process,
        // while referenced binaries are still signed and hash-checked through the signed release manifest.
        #[cfg(feature = "production")]
        if !matches!(header.trust(), cosign2::Trust::PartiallyTrusted | cosign2::Trust::FullyTrusted) {
            bail!("Manifest header pubkey is not trusted")
        }

        let version = header.version().to_string();
        log::debug!("manifest: version: {version} == {tar_version:?}");

        if *tar_version != Some(version) {
            bail!("Manifest verification failed");
        }

        Ok(serde_json::from_slice(json)?)
    }

    fn create_dir_all(&self, path: &str, location: Location) {
        let path_parts = path.split('/').collect::<Vec<_>>();
        let mut current_path = String::new();
        for path_part in path_parts {
            current_path.push_str(&format!("/{path_part}"));
            if self.fs.metadata(&current_path, location).is_err() {
                self.fs.create_dir(&current_path, location).ok();
            }
        }
    }

    pub fn tar_read_file_progress(
        &self,
        tar_name: &str,
        location: Location,
        file_name: &str,
        progress_fn: impl Fn(f32),
    ) -> anyhow::Result<(DropDeallocate, usize)> {
        log::debug!("Reading {file_name} from tar file: {tar_name} @ {location:?}");

        let file = self.fs.open_file(
            tar_name,
            location,
            fs::OpenFlags { read: true, write: false, create: false },
        )?;

        let mut archive = tar::Archive::new(file);
        for entry in archive.entries_with_seek()? {
            let entry = entry?;
            let size = entry.size() as usize;

            let archive_file_name = entry.path()?.to_string_lossy().to_string();
            if archive_file_name == file_name {
                return Ok(fw_utils::hash::read_progress(entry, size, progress_fn)?);
            }
        }

        anyhow::bail!("File {file_name} not found in the archive");
    }

    /// Extract a file from tar archive directly to filesystem in a memory-efficient way.
    /// Returns the size of the extracted file.
    pub fn tar_extract_file(
        &self,
        tar_name: &str,
        tar_location: Location,
        tar_file_name: &str,
        dst_path: &str,
        dst_location: Location,
        progress_fn: impl Fn(f32),
    ) -> anyhow::Result<usize> {
        log::debug!("Extracting {tar_file_name} from tar file: {tar_name} @ {tar_location:?} to {dst_path} @ {dst_location:?}");

        let file = self.fs.open_file(
            tar_name,
            tar_location,
            fs::OpenFlags { read: true, write: false, create: false },
        )?;

        let mut archive = tar::Archive::new(file);
        for entry in archive.entries_with_seek()? {
            let entry = entry?;
            let size = entry.size() as usize;

            let archive_file_name = entry.path()?.to_string_lossy().to_string();
            if archive_file_name == tar_file_name {
                fw_utils::hash::stream_to_file_progress(
                    &self.fs,
                    entry,
                    size,
                    dst_path,
                    dst_location,
                    progress_fn,
                )?;
                return Ok(size);
            }
        }

        anyhow::bail!("File {tar_file_name} not found in the archive");
    }
}

fn os_binary_to_file_name_and_location(os_binary: &str) -> anyhow::Result<(String, String, Location)> {
    match os_binary {
        "app.bin" => Ok((
            KEYOS_RECOVERY_OLD_FILE_PATH.to_string(),
            KEYOS_RECOVERY_FILE_PATH.to_string(),
            Location::System,
        )),
        "recovery.bin" => Ok(("recovery.old".to_string(), "recovery.new".to_string(), Location::Boot)),
        "boot.bin" => Ok(("bootb.old".to_string(), "bootb.new".to_string(), Location::Boot)),
        "boot.cip" => Ok(("bootc.old".to_string(), "bootc.new".to_string(), Location::Boot)),

        _ => bail!("Unknown OS binary name: {os_binary}"),
    }
}

/// Converts an asset path from the tar archive to the destination filesystem path
///
/// Strips the leading version adn applies location-specific fixups:
/// - `common-boot` is renamed to `common` for `Boot` assets
/// - `keyos/` prefix is inserted for `System` assets that don't already have one
fn asset_tar_path_to_fs_path(tar_asset_path: &str, asset_location: Location) -> String {
    let mut path_parts = tar_asset_path.split('/').skip(1).collect::<Vec<_>>();

    if asset_location == Location::Boot && path_parts.first().map(|p| *p == "common-boot").unwrap_or_default()
    {
        path_parts[0] = "common";
    }
    if asset_location == Location::System && !path_parts.contains(&"keyos") {
        path_parts.insert(0, "keyos");
    }

    path_parts.join("/")
}

fn asset_fs_path_to_wipe_root(fs_path: &str, asset_location: Location) -> Option<String> {
    let path_parts = fs_path.split('/').collect::<Vec<_>>();
    let root_depth = if asset_location == Location::System
        && path_parts.get(0) == Some(&"keyos")
        && path_parts.get(1) == Some(&"apps")
        && path_parts.get(3) == Some(&"resources")
    {
        4
    } else if asset_location == Location::System {
        2
    } else {
        1
    };

    let root = path_parts.into_iter().take(root_depth).collect::<Vec<_>>().join("/");
    (!root.is_empty()).then_some(root)
}

fn verify_manifest_hash(manifest: &RecoveryManifest, path: &str, hash: [u8; 32]) -> bool {
    for file in manifest.files.iter() {
        if file.name == path {
            if file.hash.len() != 32 {
                log::error!("Invalid manifest hash length: {} bytes", file.hash.len());
                return false;
            }

            let res = constant_time_eq(&file.hash, &hash);
            if !res {
                log::debug!("Hash mismatch for file {path}:");
                log::debug!("{}", hex::encode(hash));
                log::debug!("{}", hex::encode(&file.hash));
            } else {
                log::debug!("Hash OK for file {path}");
            }

            return res;
        }
    }

    false
}
