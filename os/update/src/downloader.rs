// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Write;
use std::time::{Duration, Instant};

use foundation_api::firmware::{
    ArchivedFirmwareChunk, ArchivedFirmwareFetchEvent as FwEvent, FirmwareUpdateAvailable,
};
use fs::adapter::FsAdapter;
use fs::OpenFlags;
use quantum_link::SendMessageError;
use update::{messages::DownloadProgress, DownloadError};
use whence::WhenceExt;

const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(30);
const RESYNC_RETRY_INTERVAL: Duration = Duration::from_secs(5);

pub struct UpdateDownloader<F: FsAdapter> {
    fs: F,
    state: State<F>,
}

#[derive(Default)]
enum State<F: FsAdapter> {
    #[default]
    Idle,
    InProgress {
        metadata: FirmwareUpdateAvailable,
        patches: Vec<PatchState<F>>,
        last_progress_at: Instant,
        resync: Option<Instant>,
    },
    Failed {
        stale_event_logged: bool,
    },
}

struct PatchState<F: FsAdapter> {
    file_path: String,
    file: F::File,
    chunks_received: u16,
    total_chunks: Option<u16>,
}

impl<F: FsAdapter> PatchState<F> {
    fn is_complete(&self) -> bool { self.total_chunks == Some(self.chunks_received) }
}

#[derive(Debug, Clone)]
pub struct UpdateFiles {
    #[allow(unused)]
    pub metadata: FirmwareUpdateAvailable,
    pub paths: Vec<String>,
}

#[derive(Debug)]
pub enum EventOutcome {
    None,
    Done(UpdateFiles),
    Retry { chunk_offset: u64 },
}

impl<F: FsAdapter> UpdateDownloader<F> {
    pub fn new(fs: F) -> Self { Self { fs, state: State::Idle } }

    pub fn reset_state(&mut self) { self.state = State::Idle; }

    pub fn is_downloading(&self) -> bool { matches!(self.state, State::InProgress { .. }) }

    pub fn handle_stall_monitor(&mut self) {
        {
            let State::InProgress { patches, metadata, last_progress_at, .. } = &self.state else {
                return;
            };

            let idle_for = Instant::now().saturating_duration_since(*last_progress_at);
            if idle_for < DOWNLOAD_STALL_TIMEOUT {
                return;
            }

            let progress = Self::progress_from_patches(metadata, patches);
            log::error!(
                "firmware download stalled for {:?} (chunks received: {}, total chunks: {})",
                idle_for,
                progress.chunks_received,
                progress.total_chunks
            );
        };

        self.mark_failed();
    }

    pub fn handle_event(&mut self, event: &FwEvent) -> whence::Result<EventOutcome, DownloadError>
    where
        F::Permissions: fs::adapter::BasicFsPermissions,
    {
        if let State::Failed { stale_event_logged } = &mut self.state {
            if !matches!(event, FwEvent::Starting(_) | FwEvent::UpdateNotAvailable) {
                if !*stale_event_logged {
                    let event_name = match event {
                        FwEvent::Downloading => "Downloading",
                        FwEvent::Chunk(_) => "Chunk",
                        FwEvent::Error { .. } => "Error",
                        FwEvent::UpdateNotAvailable => "UpdateNotAvailable",
                        FwEvent::Starting(_) => "Starting",
                    };
                    log::info!(
                        "ignoring stale FirmwareFetchEvent::{event_name} after a failed download attempt; waiting for Starting"
                    );
                    *stale_event_logged = true;
                }
                return Ok(EventOutcome::None);
            }
        }

        let result = (|| match event {
            FwEvent::UpdateNotAvailable => {
                log::info!("update not available");
                self.reset_state();
                Ok(EventOutcome::None)
            }
            FwEvent::Starting(metadata) => {
                if matches!(self.state, State::InProgress { .. }) {
                    log::warn!(
                        "received FirmwareFetchEvent::Starting while download is in progress; resetting local download state"
                    );
                }

                let location = fs::Location::System;
                let patches_dir = patches_dir();
                self.fs.remove(&patches_dir, location).ok();
                self.fs.ensure_parent_dir_exists(&patches_dir, location).whence()?;
                self.fs.create_dir(&patches_dir, location).whence()?;

                let mut patches = Vec::with_capacity(metadata.patch_count as usize);
                for patch_index in 0..metadata.patch_count {
                    let file_path = format!("{patches_dir}/firmware_update_{patch_index}.tar");
                    let file = self.fs.open_file(&file_path, location, OpenFlags::CREATE).whence()?;
                    patches.push(PatchState { file_path, file, chunks_received: 0, total_chunks: None });
                }

                log::info!("started firmware download: {} patches expected", metadata.patch_count);
                self.state = State::InProgress {
                    metadata: FirmwareUpdateAvailable {
                        version: metadata.version.to_string(),
                        patch_count: metadata.patch_count.into(),
                        changelog: metadata.changelog.to_string(),
                        timestamp: metadata.timestamp.into(),
                        total_size: metadata.total_size.into(),
                    },
                    patches,
                    last_progress_at: Instant::now(),
                    resync: None,
                };

                Ok(EventOutcome::None)
            }
            FwEvent::Downloading => {
                log::info!("envoy is downloading firmware");
                Ok(EventOutcome::None)
            }
            FwEvent::Chunk(chunk) => self.handle_chunk(chunk),
            FwEvent::Error { error } => {
                log::error!("envoy reported error during firmware fetch: {error}");
                Err(DownloadError::EnvoyError(error.to_string())).whence()
            }
        })();

        if result.is_err() {
            self.mark_failed();
        }

        result
    }

    pub fn handle_resume_result(
        &mut self,
        result: Result<(), SendMessageError>,
    ) -> Result<(), DownloadError> {
        let State::InProgress { resync: Some(_resync), .. } = &self.state else {
            return Ok(());
        };

        let Err(error) = result else {
            return Ok(());
        };

        log::error!("failed to request firmware download resume: {error}");
        self.mark_failed();
        Err(DownloadError::RetryFailed(error.to_string()))
    }

    fn handle_chunk(&mut self, chunk: &ArchivedFirmwareChunk) -> whence::Result<EventOutcome, DownloadError>
    where
        F::Permissions: fs::adapter::BasicFsPermissions,
    {
        let State::InProgress { patches, last_progress_at, resync, .. } = &mut self.state else {
            log::error!("received chunk but not in progress state");
            return Err(DownloadError::InvalidState).whence();
        };

        let chunk_index = chunk.chunk_index.to_native();
        let total_chunks = chunk.total_chunks.to_native();
        let patch_index: usize = (u8::from(chunk.patch_index)) as usize;
        let total_patches: usize = (u8::from(chunk.total_patches)) as usize;

        if total_patches != patches.len() {
            log::error!(
                "unexpected total_patches in chunk: expected {}, got {}",
                patches.len(),
                total_patches
            );
            return Err(DownloadError::InvalidChunk).whence();
        }
        if chunk_index >= total_chunks {
            log::error!("chunk index {} exceeds total chunks {}", chunk.chunk_index, chunk.total_chunks);
            return Err(DownloadError::InvalidChunk).whence();
        }

        let Some((expected_patch_index, expected_chunk_index)) = Self::expected_chunk_position(patches)
        else {
            log::error!("no expected chunk position");
            return Err(DownloadError::InvalidChunk).whence();
        };

        if patch_index < expected_patch_index
            || (patch_index == expected_patch_index && chunk_index < expected_chunk_index)
        {
            log::info!(
                    "ignoring duplicate chunk {chunk_index} for patch {patch_index}; expected patch {expected_patch_index} chunk {expected_chunk_index}"
                );
            return Ok(EventOutcome::None);
        }

        let expected_offset = patches.iter().map(|patch| patch.chunks_received as u64).sum();

        if patch_index != expected_patch_index || chunk_index != expected_chunk_index {
            let now = Instant::now();
            if let Some(last_resume_at) = resync {
                if now.saturating_duration_since(*last_resume_at) < RESYNC_RETRY_INTERVAL {
                    log::info!(
                        "ignoring out-of-order chunk while waiting for resume offset {expected_offset}: got patch {patch_index} chunk {chunk_index}, expected patch {expected_patch_index} chunk {expected_chunk_index}"
                    );
                    return Ok(EventOutcome::None);
                }

                log::info!("resync timeout elapsed, requesting resume again from offset {expected_offset}");
                *last_resume_at = now;
                return Ok(EventOutcome::Retry { chunk_offset: expected_offset });
            }

            log::info!("out of order chunk: expected patch {expected_patch_index} chunk {expected_chunk_index}, got patch {patch_index} chunk {chunk_index}");
            *resync = Some(now);
            return Ok(EventOutcome::Retry { chunk_offset: expected_offset });
        }

        {
            let patch = &mut patches[expected_patch_index];
            if let Some(total) = patch.total_chunks {
                if total_chunks != total {
                    log::error!(
                        "total_chunks mismatch for patch {patch_index}: expected {total}, got {total_chunks}"
                    );
                    return Err(DownloadError::InvalidChunk).whence();
                }
            } else {
                patch.total_chunks = Some(total_chunks);
            }

            patch.file.write_all(&chunk.data).whence()?;
            patch.chunks_received += 1;

            if patch.chunks_received == total_chunks {
                patch.file.flush().whence()?;
                log::info!("patch {patch_index} download complete");
            }
        }

        *last_progress_at = Instant::now();

        if resync.take().is_some() {
            log::info!("firmware stream resynchronized at chunk offset {expected_offset}");
        }

        log::debug!(
            "received chunk {}/{} for patch {}/{}",
            chunk_index + 1,
            total_chunks,
            patch_index + 1,
            total_patches
        );

        if Self::expected_chunk_position(patches).is_none() {
            self.handle_complete()
        } else {
            Ok(EventOutcome::None)
        }
    }

    fn handle_complete(&mut self) -> whence::Result<EventOutcome, DownloadError>
    where
        F::Permissions: fs::adapter::BasicFsPermissions,
    {
        let State::InProgress { ref patches, ref metadata, .. } = self.state else {
            log::error!("complete called but not in progress state");
            return Err(DownloadError::InvalidState).whence();
        };

        for (index, patch) in patches.iter().enumerate() {
            let Some(total) = patch.total_chunks else {
                log::error!("patch {index} never received any chunks");
                return Err(DownloadError::InvalidChunk).whence();
            };
            if patch.chunks_received != total {
                log::error!(
                    "patch {} incomplete: received {}/{} chunks",
                    index,
                    patch.chunks_received,
                    total
                );
                return Err(DownloadError::InvalidChunk).whence();
            }
        }

        let paths: Vec<String> = patches.iter().map(|p| p.file_path.clone()).collect();
        let metadata = metadata.clone();

        log::info!("firmware download complete: {} patches", paths.len());

        self.state = State::Idle;

        Ok(EventOutcome::Done(UpdateFiles { metadata, paths }))
    }

    pub fn get_downloading_progress(&self) -> Option<DownloadProgress> {
        match &self.state {
            State::Idle => None,
            State::InProgress { patches, metadata, .. } => {
                Some(Self::progress_from_patches(metadata, patches))
            }
            State::Failed { .. } => None,
        }
    }

    fn progress_from_patches(
        metadata: &FirmwareUpdateAvailable,
        patches: &[PatchState<F>],
    ) -> DownloadProgress {
        let total_chunks: u32 = patches.iter().map(|p| p.total_chunks.unwrap_or(0) as u32).sum();
        let chunks_received: u32 = patches.iter().map(|p| p.chunks_received as u32).sum();
        DownloadProgress {
            patches_total: metadata.patch_count as u32,
            patches_complete: patches.iter().filter(|p| p.is_complete()).count() as u32,
            chunks_received,
            total_chunks,
        }
    }

    fn expected_chunk_position(patches: &[PatchState<F>]) -> Option<(usize, u16)> {
        patches.iter().enumerate().find_map(|(patch_index, patch)| {
            (!patch.is_complete()).then_some((patch_index, patch.chunks_received))
        })
    }

    fn mark_failed(&mut self) {
        if matches!(self.state, State::Failed { .. }) {
            return;
        }

        self.state = State::Failed { stale_event_logged: false };
    }
}

pub(crate) fn patches_dir() -> String { format!("{}/patches", fs::SYSTEM_STATE_ROOT) }

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use foundation_api::firmware::{FirmwareChunk, FirmwareFetchEvent, FirmwareUpdateAvailable};
    use fs::adapter::test_utils::FsTest;
    use quantum_link::SendMessageError;

    use super::*;

    fn create_metadata(patch_count: u8) -> FirmwareUpdateAvailable {
        FirmwareUpdateAvailable {
            version: "2.0.0".to_string(),
            patch_count,
            changelog: "Test changelog".to_string(),
            timestamp: 0,
            total_size: 1000,
        }
    }

    fn create_chunk(
        patch_index: u8,
        total_patches: u8,
        chunk_index: u16,
        total_chunks: u16,
        data: Vec<u8>,
    ) -> FirmwareChunk {
        FirmwareChunk { patch_index, total_patches, chunk_index, total_chunks, data }
    }

    struct TestDownloader(UpdateDownloader<FsTest>);
    impl Default for TestDownloader {
        fn default() -> Self { Self(UpdateDownloader::new(Default::default())) }
    }

    impl TestDownloader {
        fn handle_event(&mut self, event: FirmwareFetchEvent) -> whence::Result<EventOutcome, DownloadError> {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event).unwrap();
            let archived = rkyv::access::<FwEvent, rkyv::rancor::Error>(&bytes).unwrap();
            self.0.handle_event(archived)
        }

        fn handle_resume_result(
            &mut self,
            result: Result<(), SendMessageError>,
        ) -> Result<(), DownloadError> {
            self.0.handle_resume_result(result)
        }

        pub fn get_downloading_progress(&self) -> Option<DownloadProgress> {
            self.0.get_downloading_progress()
        }

        fn is_downloading(&self) -> bool { self.0.is_downloading() }

        fn handle_stall_monitor(&mut self) { self.0.handle_stall_monitor() }

        fn set_idle_for(&mut self, idle_for: Duration) {
            let State::InProgress { last_progress_at, .. } = &mut self.0.state else {
                panic!("downloader not in progress");
            };
            *last_progress_at = Instant::now() - idle_for;
        }

        fn set_resync_idle_for(&mut self, idle_for: Duration) {
            let State::InProgress { resync, .. } = &mut self.0.state else {
                panic!("downloader not in progress");
            };

            let Some(last_resume_at) = resync else {
                panic!("downloader not in resync state");
            };

            *last_resume_at = Instant::now() - idle_for;
        }
    }

    fn assert_noop(outcome: EventOutcome) {
        assert!(matches!(outcome, EventOutcome::None));
    }

    fn assert_retry(outcome: EventOutcome, expected_offset: u64) {
        assert!(matches!(outcome, EventOutcome::Retry { chunk_offset } if chunk_offset == expected_offset));
    }

    fn unwrap_done(outcome: EventOutcome) -> UpdateFiles {
        match outcome {
            EventOutcome::Done(update_files) => update_files,
            _ => panic!("expected EventOutcome::Done"),
        }
    }

    #[test]
    fn basic_single_patch_flow() {
        let mut downloader = TestDownloader::default();

        assert!(downloader.get_downloading_progress().is_none());

        let metadata = create_metadata(1);
        assert_noop(downloader.handle_event(FirmwareFetchEvent::Starting(metadata)).unwrap());

        let progress = downloader.get_downloading_progress().unwrap();
        assert_eq!(progress.patches_total, 1);
        assert_eq!(progress.patches_complete, 0);

        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3])))
                .unwrap(),
        );

        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 3, vec![4, 5, 6])))
                .unwrap(),
        );

        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 2, 3, vec![7, 8, 9])));
        let outcome = result.unwrap();
        let update_files = unwrap_done(outcome);
        assert_eq!(update_files.paths.len(), 1);
        assert!(update_files.paths[0].contains("firmware_update_0.tar"));

        let progress = downloader.get_downloading_progress();

        // should be None after completion
        assert!(progress.is_none());
    }

    #[test]
    fn multi_patch_flow() {
        let mut downloader = TestDownloader::default();

        let metadata = create_metadata(2);
        downloader.handle_event(FirmwareFetchEvent::Starting(metadata)).unwrap();

        downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 2, 0, 2, vec![1, 2]))).unwrap();
        downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 2, 1, 2, vec![3, 4]))).unwrap();

        let result = downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(1, 2, 0, 1, vec![5, 6])));
        let update_files = unwrap_done(result.unwrap());
        assert_eq!(update_files.paths.len(), 2);
        assert_eq!(update_files.metadata.patch_count, 2);
    }

    #[test]
    fn out_of_order_chunk_requests_resume() {
        let mut downloader = TestDownloader::default();

        let metadata = create_metadata(1);
        downloader.handle_event(FirmwareFetchEvent::Starting(metadata)).unwrap();

        // skip chunk 0, send chunk 1
        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 3, vec![1, 2, 3])));
        assert_retry(result.unwrap(), 0);
    }

    #[test]
    fn resume_request_is_single_flight_until_response() {
        let mut downloader = TestDownloader::default();
        downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(1))).unwrap();

        let first =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 3, vec![1, 2, 3])));
        assert_retry(first.unwrap(), 0);

        let second =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 2, 3, vec![4, 5, 6])));
        assert_noop(second.unwrap());

        // still waiting for the expected chunk 0; avoid duplicate resume requests.
        let third =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 2, 3, vec![4, 5, 6])));
        assert_noop(third.unwrap());

        // once we receive the expected chunk, resync is cleared.
        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![7, 8, 9])))
                .unwrap(),
        );

        let next_resync =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 2, 3, vec![4, 5, 6])));
        assert_retry(next_resync.unwrap(), 1);
    }

    #[test]
    fn resume_request_retries_after_resync_timeout() {
        let mut downloader = TestDownloader::default();
        downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(1))).unwrap();

        let first =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 3, vec![1, 2, 3])));
        assert_retry(first.unwrap(), 0);

        downloader.set_resync_idle_for(RESYNC_RETRY_INTERVAL + Duration::from_millis(1));

        let retry =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 2, 3, vec![4, 5, 6])));
        assert_retry(retry.unwrap(), 0);
    }

    #[test]
    fn duplicate_chunk_is_ignored() {
        let mut downloader = TestDownloader::default();

        downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(1))).unwrap();
        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3])))
                .unwrap(),
        );

        let duplicate =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3])));
        assert_noop(duplicate.unwrap());
    }

    #[test]
    fn chunk_before_start_fails() {
        let mut downloader = TestDownloader::default();

        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3])));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err().error, DownloadError::InvalidState));
    }

    #[test]
    fn envoy_error() {
        let mut downloader = TestDownloader::default();

        let result = downloader.handle_event(FirmwareFetchEvent::Error { error: "test error".to_string() });
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err().error, DownloadError::EnvoyError(_)));
    }

    #[test]
    fn invalid_patch_index_requests_resume() {
        let mut downloader = TestDownloader::default();

        let metadata = create_metadata(1);
        downloader.handle_event(FirmwareFetchEvent::Starting(metadata)).unwrap();

        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(5, 1, 0, 3, vec![1, 2, 3])));
        assert_retry(result.unwrap(), 0);
    }

    #[test]
    fn total_patches_mismatch_is_fatal() {
        let mut downloader = TestDownloader::default();
        downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(1))).unwrap();

        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 2, 0, 3, vec![1, 2, 3])));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err().error, DownloadError::InvalidChunk));
    }

    #[test]
    fn total_chunks_mismatch_is_fatal() {
        let mut downloader = TestDownloader::default();
        downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(1))).unwrap();

        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3])))
                .unwrap(),
        );

        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 4, vec![4, 5, 6])));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err().error, DownloadError::InvalidChunk));
    }

    #[test]
    fn chunk_index_exceeds_total_is_fatal() {
        let mut downloader = TestDownloader::default();
        downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(1))).unwrap();

        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 0, vec![1, 2, 3])));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err().error, DownloadError::InvalidChunk));
    }

    #[test]
    fn downloading_event_is_noop() {
        let mut downloader = TestDownloader::default();

        assert_noop(downloader.handle_event(FirmwareFetchEvent::Downloading).unwrap());
    }

    #[test]
    fn resync_recovers_when_expected_chunk_arrives() {
        let mut downloader = TestDownloader::default();

        let metadata = create_metadata(1);
        downloader.handle_event(FirmwareFetchEvent::Starting(metadata)).unwrap();

        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3])))
                .unwrap(),
        );

        let resync =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 2, 3, vec![7, 8, 9])));
        assert_retry(resync.unwrap(), 1);

        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 3, vec![4, 5, 6])))
                .unwrap(),
        );

        let done =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 2, 3, vec![7, 8, 9])));
        let _ = unwrap_done(done.unwrap());
    }

    #[test]
    fn resume_error_transitions_to_failed_state() {
        let mut downloader = TestDownloader::default();
        downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(1))).unwrap();

        let resync =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 3, vec![1, 2, 3])));
        assert_retry(resync.unwrap(), 0);

        let error = downloader.handle_resume_result(Err(SendMessageError::Timeout));
        assert!(matches!(error, Err(DownloadError::RetryFailed(_))));
        assert!(matches!(downloader.0.state, State::Failed { .. }));
    }

    #[test]
    fn stale_events_are_ignored_after_failure_until_restart() {
        let mut downloader = TestDownloader::default();
        let metadata = create_metadata(1);
        downloader.handle_event(FirmwareFetchEvent::Starting(metadata.clone())).unwrap();

        let resync =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 3, vec![1, 2, 3])));
        assert_retry(resync.unwrap(), 0);
        assert!(downloader.handle_resume_result(Err(SendMessageError::Cancelled)).is_err());

        // Subsequent stale events from this attempt are ignored.
        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 2, 3, vec![4, 5, 6])));
        assert_noop(result.unwrap());
        let result = downloader.handle_event(FirmwareFetchEvent::Downloading);
        assert_noop(result.unwrap());
        let result = downloader.handle_event(FirmwareFetchEvent::Error { error: "ignored".to_string() });
        assert_noop(result.unwrap());

        // A fresh start exits failed state and accepts chunk 0 again.
        assert_noop(downloader.handle_event(FirmwareFetchEvent::Starting(metadata)).unwrap());
        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![7, 8, 9])));
        assert_noop(result.unwrap());
    }

    #[test]
    fn update_not_available_clears_failed_state() {
        let mut downloader = TestDownloader::default();
        let metadata = create_metadata(1);
        downloader.handle_event(FirmwareFetchEvent::Starting(metadata)).unwrap();

        let resync =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 3, vec![1, 2, 3])));
        assert_retry(resync.unwrap(), 0);
        assert!(downloader.handle_resume_result(Err(SendMessageError::Cancelled)).is_err());

        let result = downloader.handle_event(FirmwareFetchEvent::UpdateNotAvailable);
        assert_noop(result.unwrap());

        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![4, 5, 6])));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err().error, DownloadError::InvalidState));
    }

    #[test]
    fn late_resume_error_is_ignored_after_recovery() {
        let mut downloader = TestDownloader::default();
        downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(1))).unwrap();

        let resync =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 1, 3, vec![4, 5, 6])));
        assert_retry(resync.unwrap(), 0);

        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3])))
                .unwrap(),
        );

        let late = downloader.handle_resume_result(Err(SendMessageError::Cancelled));
        assert!(late.is_ok());
        assert!(downloader.is_downloading());
    }

    #[test]
    fn starting_resets_in_progress_download() {
        let mut downloader = TestDownloader::default();
        downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(1))).unwrap();

        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3])))
                .unwrap(),
        );

        assert_noop(downloader.handle_event(FirmwareFetchEvent::Starting(create_metadata(2))).unwrap());

        let progress = downloader.get_downloading_progress().unwrap();
        assert_eq!(progress.patches_total, 2);
        assert_eq!(progress.chunks_received, 0);

        assert_noop(
            downloader
                .handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 2, 0, 1, vec![4, 5, 6])))
                .unwrap(),
        );
        let done =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(1, 2, 0, 1, vec![7, 8, 9])));
        let metadata = unwrap_done(done.unwrap()).metadata;
        assert_eq!(metadata.patch_count, 2);
    }

    #[test]
    fn stall_timeout_transitions_to_failed_state() {
        let mut downloader = TestDownloader::default();
        let metadata = create_metadata(1);
        downloader.handle_event(FirmwareFetchEvent::Starting(metadata.clone())).unwrap();
        assert!(downloader.is_downloading());

        downloader.set_idle_for(DOWNLOAD_STALL_TIMEOUT - Duration::from_secs(1));
        downloader.handle_stall_monitor();
        assert!(downloader.is_downloading());

        downloader.set_idle_for(DOWNLOAD_STALL_TIMEOUT);
        downloader.handle_stall_monitor();
        assert!(matches!(downloader.0.state, State::Failed { .. }));

        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3])));
        assert_noop(result.unwrap());

        downloader.handle_event(FirmwareFetchEvent::Starting(metadata)).unwrap();
        let result =
            downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![4, 5, 6])));
        assert_noop(result.unwrap());
    }

    #[test]
    fn chunk_progress_resets_stall_timeout() {
        let mut downloader = TestDownloader::default();
        let metadata = create_metadata(1);
        downloader.handle_event(FirmwareFetchEvent::Starting(metadata)).unwrap();

        downloader.set_idle_for(Duration::from_secs(60));
        downloader.handle_event(FirmwareFetchEvent::Chunk(create_chunk(0, 1, 0, 3, vec![1, 2, 3]))).unwrap();

        downloader.handle_stall_monitor();
        assert!(downloader.is_downloading());

        downloader.set_idle_for(DOWNLOAD_STALL_TIMEOUT);
        downloader.handle_stall_monitor();
        assert!(matches!(downloader.0.state, State::Failed { .. }));
    }

    #[test]
    fn idle_downloader_never_reports_stall() {
        let mut downloader = TestDownloader::default();
        downloader.handle_stall_monitor();
        assert!(matches!(downloader.0.state, State::Idle));
    }
}
