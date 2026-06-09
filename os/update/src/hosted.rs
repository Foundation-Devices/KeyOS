// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::Owned;
use update::messages::*;

use crate::DownloadStallTick;

#[derive(Default, server::Server)]
#[name = "os/update"]
pub struct Server {
    subscribers: Vec<server::ArchiveEventSubscriber<ProgressUpdate>>,
}

impl Server {
    pub fn new(_sid: xous::SID) -> Self { Self::default() }

    fn notify(&mut self, event: ProgressUpdate) { self.subscribers.retain(|s| s.send(&event).is_ok()); }
}

impl server::Server for Server {}

impl server::ArchiveEventSubscriptionHandler<SubscribeUpdateProgress> for Server {
    fn handle(
        &mut self,
        _msg: SubscribeUpdateProgress,
        subscriber: server::ArchiveEventSubscriber<ProgressUpdate>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.subscribers.push(subscriber);
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
        let mut patches = vec![];
        for _ in &msg.release_paths {
            patches.push(PatchProgress {
                file_size: 1024 * 1024, // 1MB simulated
                total_actions: 10,
                completed_actions: 0,
                requires_reboot: false,
            });
        }

        let mut progress = InstallProgress {
            patches,
            firmware_copy: FirmwareCopyProgress { copied_bytes: 0, total_bytes: 100 },
        };

        self.notify(ProgressUpdate::InstallProgress(progress.clone()));

        for release in msg.release_paths {
            log::info!("Hosted update: simulating update from path: {release}");

            progress.set_firmware_copy(FirmwareCopyProgress { copied_bytes: 100, total_bytes: 100 });
            self.notify(ProgressUpdate::InstallProgress(progress.clone()));

            let steps_total = 10;
            for ii in 0..steps_total {
                std::thread::sleep(std::time::Duration::from_millis(500));
                progress.action_completed();
                self.notify(ProgressUpdate::InstallProgress(progress.clone()));

                log::info!("Hosted update: step {}/{} completed", ii + 1, steps_total);
            }

            self.notify(ProgressUpdate::InstallProgress(progress.clone()));
            log::info!("Hosted update: release applied successfully");
        }

        log::info!("Hosted update: simulation complete");
    }
}

impl server::ArchiveHandler<ContinueUpdate> for Server {
    fn handle(
        &mut self,
        _msg: Owned<ContinueUpdate>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::info!("Hosted update: continuing update (no-op in hosted mode)");
    }
}

impl server::BlockingArchiveHandler<FirmwareVersion> for Server {
    fn handle(
        &mut self,
        _msg: FirmwareVersion,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <FirmwareVersion as server::BlockingArchive>::Response {
        Ok("hosted-1.0.0".to_string())
    }
}

impl server::ArchiveHandler<ApplyDownloadedUpdate> for Server {
    fn handle(
        &mut self,
        _msg: Owned<ApplyDownloadedUpdate>,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::info!("Hosted update: applying downloaded update (no-op in hosted mode)");
    }
}

impl server::BlockingScalarHandler<GetUpdateApplied> for Server {
    fn handle(
        &mut self,
        _msg: GetUpdateApplied,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetUpdateApplied as server::BlockingScalar>::Response {
        false
    }
}

impl server::ScalarHandler<ClearUpdateApplied> for Server {
    fn handle(
        &mut self,
        _msg: ClearUpdateApplied,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::info!("Hosted update: clear_update_applied (no-op in hosted mode)");
    }
}

impl server::BlockingArchiveHandler<GetUpdateStatus> for Server {
    fn handle(
        &mut self,
        _msg: GetUpdateStatus,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetUpdateStatus as server::BlockingArchive>::Response {
        UpdateStatus { downloaded_update: false, needs_continue: false, sufficient_battery: true }
    }
}

impl server::ScalarHandler<DownloadStallTick> for Server {
    fn handle(
        &mut self,
        _msg: DownloadStallTick,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
    }
}
