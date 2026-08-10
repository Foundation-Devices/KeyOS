// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Read;
use std::thread;
use std::time::Duration;

use file_backed::JsonBacked;
use foundation_api::firmware::FirmwareFetchEvent;
use server::{xous, ArchiveSubList, MessageId as _, Owned};
use update::messages::InstallProgress;
use update::{messages::*, Error, MIN_UPDATE_BATTERY_PERCENT};
use whence::WhenceExt;
use xous_ticktimer::TicktimerCallback;

use crate::core::{UpdateEvent, UpdateOutcome, FIRMWARE_FILE_PATH, STAGED_FIRMWARE_FILE_PATH};
use crate::downloader::{EventOutcome, UpdateDownloader};
use crate::fs_permissions::FileSystemPermissions;
use crate::state::{DownloadedUpdate, UpdateState};
use crate::{
    core, CryptoApi, DownloadStallTick, FileSystem, GuiApiLight, PowerManagerExtApi, QuantumLinkApi, Security,
};

const DOWNLOAD_STALL_TICK_INTERVAL: Duration = Duration::from_secs(1);

#[derive(server::Server)]
#[name = "os/update"]
pub struct Server {
    fs: FileSystem,
    gui: GuiApiLight,
    ql: QuantumLinkApi,
    power_manager: PowerManagerExtApi,
    state: JsonBacked<UpdateState, FileSystemPermissions>,
    downloader: UpdateDownloader<FileSystem>,
    progress_subscribers: ArchiveSubList<ProgressUpdate>,
    download_stall_cb: TicktimerCallback,
    install_running: bool,
    sender: ServerSender,
}

impl Server {
    pub fn new(sid: xous::SID) -> Self {
        let fs = FileSystem::default();
        let mut state = UpdateState::load();
        if let Err(e) = clear_state_after_recovery(&fs, &mut state) {
            if e.marker_observed {
                // The state itself is cleared either way; a surviving marker only means we
                // redo this on the next boot.
                log::error!("failed to remove the Recovery update-state marker: {:?}", e.error);
            } else {
                log::error!("failed to check Recovery update-state marker: {:?}", e.error);
            }
        }
        let downloader = UpdateDownloader::new(fs.clone());
        let download_stall_cb = TicktimerCallback::new(sid).expect("could not register callback");
        Self {
            fs,
            gui: GuiApiLight::default(),
            ql: QuantumLinkApi::default(),

            power_manager: Default::default(),
            state,
            downloader,
            progress_subscribers: Default::default(),
            download_stall_cb,
            install_running: false,
            sender: ServerSender::new(sid),
        }
    }
}

fn clear_state_after_recovery(
    fs: &FileSystem,
    state: &mut JsonBacked<UpdateState, FileSystemPermissions>,
) -> Result<(), crate::state::ConsumeUpdateStateInvalidatedMarkerError> {
    let consumed =
        crate::state::consume_update_state_invalidated_marker(fs, || reset_update_state(fs, state))?;

    if consumed {
        log::info!("Recovery detected, cleared update state");
    }

    Ok(())
}

fn reset_update_state(fs: &FileSystem, state: &mut JsonBacked<UpdateState, FileSystemPermissions>) {
    let pending_release_paths = state.pending_apply.clone();
    remove_release_files(fs, &pending_release_paths);

    let mut state = state.guard();
    **state = UpdateState::default();
}

fn remove_release_files(fs: &FileSystem, release_paths: &[String]) {
    for release_path in release_paths {
        match fs.remove(release_path.as_str(), fs::Location::System) {
            Ok(()) | Err(fs::Error::FileNotFound) => {}
            Err(e) => {
                log::warn!("failed to remove abandoned update release file {release_path}: {e:?}");
            }
        }
    }
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
enum FirmwareInstallWorkerEvent {
    Progress(ProgressUpdate),
    Complete(UpdateOutcome),
    Error(Error),
}

#[derive(Clone)]
struct ServerSender {
    conn: server::CheckedConn<server::AllPermissions>,
}

impl ServerSender {
    fn new(sid: xous::SID) -> Self { Self { conn: xous::connect(sid).unwrap().into() } }

    fn progress(&self, event: ProgressUpdate) { self.event(FirmwareInstallWorkerEvent::Progress(event)); }

    fn outcome(&self, outcome: UpdateOutcome) { self.event(FirmwareInstallWorkerEvent::Complete(outcome)); }

    fn error(&self, error: Error) { self.event(FirmwareInstallWorkerEvent::Error(error)); }

    fn event(&self, event: FirmwareInstallWorkerEvent) {
        self.conn
            .try_send_archive(event)
            .inspect_err(|e| log::warn!("failed to send install event {e:?}"))
            .ok();
    }
}

impl server::Server for Server {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        self.ql.subscribe_firmware_fetch(context);
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeUpdateProgress> for Server {
    fn handle(
        &mut self,
        _msg: SubscribeUpdateProgress,
        subscriber: server::ArchiveEventSubscriber<ProgressUpdate>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.progress_subscribers.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveHandler<StartUpdate> for Server {
    fn handle(
        &mut self,
        msg: Owned<StartUpdate>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let Ok(msg) = msg.deserialize() else { return };
        if let Err(e) = self.start_update(msg.release_paths) {
            log::error!("start_update failed: {e:?}");
            self.notify(ProgressUpdate::InstallError(e.into_inner()));
        }
    }
}

impl server::ArchiveHandler<ContinueUpdate> for Server {
    fn handle(
        &mut self,
        _msg: Owned<ContinueUpdate>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if let Err(e) = self.continue_update() {
            log::error!("continue_update failed: {e:?}");
            self.notify(ProgressUpdate::InstallError(e.into_inner()));
        }
    }
}

impl server::BlockingArchiveHandler<FirmwareVersion> for Server {
    fn handle(
        &mut self,
        _msg: FirmwareVersion,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <FirmwareVersion as server::BlockingArchive>::Response {
        self.firmware_version().map_err(|e| {
            log::error!("firmware_version failed: {e:?}");
            e.into_inner()
        })
    }
}

impl server::ArchiveHandler<ApplyDownloadedUpdate> for Server {
    fn handle(
        &mut self,
        _msg: Owned<ApplyDownloadedUpdate>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if let Err(e) = self.apply_downloaded_update() {
            log::error!("apply_downloaded_update failed: {e:?}");
            self.notify(ProgressUpdate::InstallError(e.into_inner()));
        }
    }
}

impl server::BlockingScalarHandler<GetUpdateApplied> for Server {
    fn handle(
        &mut self,
        _msg: GetUpdateApplied,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetUpdateApplied as server::BlockingScalar>::Response {
        self.state.update_applied
    }
}

impl server::ScalarHandler<ClearUpdateApplied> for Server {
    fn handle(
        &mut self,
        _msg: ClearUpdateApplied,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.state.guard().update_applied = false;
    }
}

impl server::BlockingArchiveHandler<GetUpdateStatus> for Server {
    fn handle(
        &mut self,
        _msg: GetUpdateStatus,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetUpdateStatus as server::BlockingArchive>::Response {
        let downloaded_update = self.state.downloaded.is_some();
        let needs_continue = !self.state.pending_apply.is_empty();
        let installing = self.install_running;
        let sufficient_battery = self.has_sufficient_battery();

        UpdateStatus { downloaded_update, needs_continue, installing, sufficient_battery }
    }
}

impl server::ArchiveEventHandler<FirmwareFetchEvent> for Server {
    fn handle(
        &mut self,
        event: Owned<FirmwareFetchEvent>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if self.install_running {
            log::info!("ignoring firmware fetch event while firmware install is in progress");
            return;
        }

        let result = self.downloader.handle_event(&*event);
        self.refresh_download_stall_tick();

        match result {
            Ok(outcome) => match outcome {
                EventOutcome::Retry { chunk_offset } => {
                    if let Err(error) = self.request_resume(chunk_offset) {
                        log::error!("firmware download resume failed: {error:?}");
                        self.notify(ProgressUpdate::DownloadError(error));
                        self.refresh_download_stall_tick();
                    }
                }
                EventOutcome::Done(update_files) => {
                    log::info!("firmware download complete, storing paths");
                    let downloaded = DownloadedUpdate { paths: update_files.paths };
                    self.state.guard().downloaded = Some(downloaded);
                    self.notify(ProgressUpdate::DownloadComplete);
                }
                EventOutcome::None => {
                    if let Some(progress) = self.downloader.get_downloading_progress() {
                        self.notify(ProgressUpdate::DownloadProgress(progress));
                    }
                }
            },
            Err(e) => {
                log::error!("firmware download failed: {e:?}");
                self.notify(ProgressUpdate::DownloadError(e.into_inner()));
            }
        }
    }
}

impl server::ScalarHandler<DownloadStallTick> for Server {
    fn handle(
        &mut self,
        _msg: DownloadStallTick,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let was_downloading = self.downloader.is_downloading();
        self.downloader.handle_stall_monitor();
        if was_downloading && !self.downloader.is_downloading() {
            self.notify(ProgressUpdate::DownloadError(update::DownloadError::Stalled));
        }
        self.refresh_download_stall_tick();
    }
}

impl server::ArchiveHandler<FirmwareInstallWorkerEvent> for Server {
    fn handle(
        &mut self,
        event: Owned<FirmwareInstallWorkerEvent>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        let Ok(event) = event.deserialize() else { return };
        match event {
            FirmwareInstallWorkerEvent::Progress(event) => {
                self.notify(event);
            }
            FirmwareInstallWorkerEvent::Complete(outcome) => {
                self.install_running = false;
                let result: whence::Result<(), Error> = (|| {
                    match outcome {
                        UpdateOutcome::Done => {
                            core::finalize_update(&mut self.fs)?;
                            self.notify_and_reboot(ProgressUpdate::Done)?;
                        }
                        UpdateOutcome::Partial(remaining_release_paths) => {
                            log::info!("release requires a reboot, saving remaining releases and rebooting");
                            let expected_version = self.firmware_version_at(STAGED_FIRMWARE_FILE_PATH)?;
                            self.save_resume_state(remaining_release_paths, expected_version);
                            core::finalize_update(&mut self.fs)?;
                            self.notify_and_reboot(ProgressUpdate::Rebooting)?;
                        }
                    }

                    Ok(())
                })();

                if let Err(e) = result {
                    log::error!("finish_update failed: {e:?}");
                    let error = e.into_inner();
                    self.notify(ProgressUpdate::InstallError(error));
                }
            }
            FirmwareInstallWorkerEvent::Error(error) => {
                self.install_running = false;
                self.notify(ProgressUpdate::InstallError(error));
            }
        }
    }
}

impl Server {
    fn request_resume(&mut self, chunk_offset: u64) -> Result<(), update::DownloadError> {
        log::info!("requesting firmware download resume from chunk offset {chunk_offset}");
        let result = self.ql.start_firmware_update(Some(chunk_offset));
        self.downloader.handle_resume_result(result)
    }

    fn refresh_download_stall_tick(&self) {
        if self.downloader.is_downloading() {
            self.download_stall_cb.request(
                DOWNLOAD_STALL_TICK_INTERVAL.as_millis() as usize,
                DownloadStallTick::ID,
                0,
            );
        } else {
            self.download_stall_cb.cancel(DownloadStallTick::ID);
        }
    }

    fn start_update(&mut self, release_paths: Vec<String>) -> whence::Result<(), Error> {
        if !self.can_start_new_update("start_update")? {
            return Ok(());
        }

        log::info!("starting firmware update procedure");

        self.spawn_apply_releases(release_paths);

        Ok(())
    }

    /// Continue an update that was interrupted by a reboot. This function assumes that
    /// pending_apply is non-empty.
    fn continue_update(&mut self) -> whence::Result<(), Error> {
        if !self.can_start_install("continue_update")? {
            return Ok(());
        }

        log::info!("continuing previous firmware update procedure");

        if self.state.finalize_update_pending {
            core::finalize_update(&mut self.fs)?;
            self.state.guard().finalize_update_pending = false;
        }

        let expected_version = self.state.expected_version_on_resume.clone();
        if let Some(expected_version) = expected_version {
            let actual_version = self.firmware_version()?;
            if actual_version != expected_version {
                log::error!(
                    "firmware version mismatch after reboot: expected {expected_version}, actual {actual_version}"
                );
                let abandoned_release_paths = self.state.pending_apply.clone();
                remove_release_files(&self.fs, &abandoned_release_paths);
                {
                    let mut state = self.state.guard();
                    state.expected_version_on_resume = None;
                    state.finalize_update_pending = false;
                    state.pending_apply.clear();
                }
                return Err(Error::UnexpectedVersionAfterReboot {
                    expected: expected_version,
                    actual: actual_version,
                })
                .whence();
            }
        }

        let remaining_release_paths = {
            let mut state = self.state.guard();
            state.expected_version_on_resume = None;
            state.finalize_update_pending = false;
            std::mem::take(&mut state.pending_apply)
        };

        self.spawn_apply_releases(remaining_release_paths);

        Ok(())
    }

    fn apply_downloaded_update(&mut self) -> whence::Result<(), Error> {
        if !self.can_start_new_update("apply_downloaded_update")? {
            return Ok(());
        }

        let Some(downloaded) = self.state.guard().downloaded.take() else {
            log::error!("no downloaded update to apply");
            return Err(Error::NoUpdateDownloaded).whence();
        };

        log::info!("applying downloaded update with {} patches", downloaded.paths.len());

        self.spawn_apply_releases(downloaded.paths);

        Ok(())
    }

    fn firmware_version(&self) -> whence::Result<String, Error> {
        self.firmware_version_at(FIRMWARE_FILE_PATH)
    }

    fn firmware_version_at(&self, firmware_path: &str) -> whence::Result<String, Error> {
        let mut firmware_file =
            self.fs.open_file(firmware_path, fs::Location::System, fs::OpenFlags::READ_ONLY).whence()?;
        let mut data = vec![0; cosign2::Header::DEFAULT_SIZE];
        firmware_file.read_exact(&mut data).whence()?;
        let header = cosign2::Header::parse_unverified(&data, cosign2::Header::DEFAULT_SIZE, false)
            .map_err(|e| Error::Cosign2(e.to_string()))
            .whence()?
            .ok_or(Error::Cosign2HeaderMissing)
            .whence()?;
        Ok(header.version().to_owned())
    }

    fn save_resume_state(&mut self, pending_apply: Vec<String>, expected_version_on_resume: String) {
        let mut state = self.state.guard();
        state.pending_apply = pending_apply;
        state.expected_version_on_resume = Some(expected_version_on_resume);
        state.finalize_update_pending = true;
    }

    fn spawn_apply_releases(&mut self, release_paths: Vec<String>) {
        self.install_running = true;
        self.downloader.reset_state();
        self.refresh_download_stall_tick();

        let sender = self.sender.clone();
        let task = InstallTask {
            fs: self.fs.clone(),
            crypto: CryptoApi::default(),
            security: Security::default(),
            sender: sender.clone(),
        };

        thread::spawn(move || match task.apply_releases(release_paths) {
            Ok(outcome) => sender.outcome(outcome),
            Err(e) => {
                log::error!("apply_releases failed: {e:?}");
                let error = e.into_inner();
                sender.error(error);
            }
        });
    }

    fn notify_and_reboot(&mut self, update: ProgressUpdate) -> whence::Result<(), Error> {
        if matches!(update, ProgressUpdate::Done) {
            self.state.guard().update_applied = true;
        }
        self.notify(update);
        // give subscribers time to process event
        std::thread::sleep(Duration::from_secs(3));

        self.gui.reboot().map_err(|_| Error::Reboot).whence()?;
        Ok(())
    }

    fn notify(&mut self, event: ProgressUpdate) { self.progress_subscribers.send_nowait(&event); }

    fn can_start_new_update(&self, operation: &str) -> whence::Result<bool, Error> {
        if !self.can_start_install(operation)? {
            return Ok(false);
        }

        if !self.state.pending_apply.is_empty() {
            log::warn!("{operation} ignored: previous update should be continued");
            return Ok(false);
        }

        Ok(true)
    }

    fn can_start_install(&self, operation: &str) -> whence::Result<bool, Error> {
        if self.install_running {
            log::warn!("{operation} ignored: update already in progress");
            return Ok(false);
        }

        if !self.has_sufficient_battery() {
            return Err(Error::InsufficientBattery.into());
        }

        Ok(true)
    }

    fn has_sufficient_battery(&self) -> bool {
        self.power_manager.status().map(|s| s.battery_percent >= MIN_UPDATE_BATTERY_PERCENT).unwrap_or(false)
    }
}

struct InstallTask {
    fs: FileSystem,
    crypto: CryptoApi,
    security: Security,
    sender: ServerSender,
}

impl InstallTask {
    /// Applies a series of releases to the update directory.
    ///
    /// If a release requires a reboot, the remaining releases will be saved
    /// and a system reboot will be initiated.
    fn apply_releases(&self, release_paths: Vec<String>) -> whence::Result<UpdateOutcome, Error> {
        let current_fw_timestamp: u32 =
            self.security.firmware_timestamp().map(u32::from).map_err(|_| Error::SecurityError).whence()?;
        let mut min_allowed_update_timestamp = current_fw_timestamp;

        let patches = core::analyze_patches(&self.fs, &release_paths)?;
        let total_bytes = core::measure_fw_size(&self.fs)?;

        let mut progress =
            InstallProgress { patches, firmware_copy: FirmwareCopyProgress { copied_bytes: 0, total_bytes } };
        self.sender.progress(ProgressUpdate::InstallProgress(progress.clone()));

        core::make_firmware_copy(&self.fs, |copied| {
            progress.firmware_copy.copied_bytes = copied;
            let event = ProgressUpdate::InstallProgress(progress.clone());
            self.sender.progress(event);
        })?;

        progress.set_firmware_copy(FirmwareCopyProgress { copied_bytes: total_bytes, total_bytes });
        self.sender.progress(ProgressUpdate::InstallProgress(progress.clone()));

        let fs = &self.fs;
        let crypto = &self.crypto;
        let sender = &self.sender;

        let mut fw_timestamp = None;

        let outcome = core::apply_update(
            fs,
            |path| {
                let header =
                    fw_utils::hash::verify_cosign2(fs, crypto, path, fs::Location::System, |_| (), false)
                        .map_err(hash_error_to_error)
                        .whence()?;
                // The update image itself is allowed be single-signed for simplicity
                // of the release process, but the contents will be double signed.
                #[cfg(feature = "production")]
                if !matches!(header.trust(), cosign2::Trust::PartiallyTrusted | cosign2::Trust::FullyTrusted,)
                {
                    return Err(Error::Cosign2("Signer public key not trusted".into())).whence();
                }

                let update_timestamp = header.timestamp();
                if update_timestamp < min_allowed_update_timestamp {
                    log::error!(
                        "rollback prevented while verifying {path}: current timestamp = {min_allowed_update_timestamp}, update timestamp = {update_timestamp}"
                    );
                    return Err(Error::RollbackPrevented {
                        current: min_allowed_update_timestamp,
                        update: update_timestamp,
                    })
                    .whence();
                }

                min_allowed_update_timestamp = update_timestamp;
                fw_timestamp = Some(update_timestamp.into());
                Ok(())
            },
            release_paths,
            |event| {
                match event {
                    UpdateEvent::ActionCompleted { .. } => {
                        progress.action_completed();
                    }
                    UpdateEvent::PatchCompleted { .. } => {}
                }
                let event = ProgressUpdate::InstallProgress(progress.clone());
                sender.progress(event);
            },
        )?;

        let Some(fw_timestamp) = fw_timestamp else {
            log::error!("firmware timestamp wasn't set");
            return Err(Error::Cosign2HeaderMissing).whence();
        };

        self.security.set_firmware_timestamp(fw_timestamp).map_err(|_| Error::SecurityError)?;

        Ok(outcome)
    }
}

fn hash_error_to_error(e: fw_utils::hash::HashError) -> Error {
    match e {
        fw_utils::hash::HashError::CryptoError(crypto) => Error::CryptoError(crypto),
        fw_utils::hash::HashError::Cosign2Error(cosign2) => Error::Cosign2(cosign2.to_string()),
        fw_utils::hash::HashError::MissingCosign2Header => Error::Cosign2HeaderMissing,
        _ => Error::Unexpected(e.to_string()),
    }
}
