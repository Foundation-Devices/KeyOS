// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use backup::{messages::*, Status};
use file_backed::FileBacked;
use quantum_link::foundation_api::backup::{ArchivedRestoreMagicBackupEvent, RestoreMagicBackupEvent};
use server::{xous, Owned};
use server::{
    ArchiveEventSubscriber, ArchiveEventSubscriptionHandler, ArchiveHandler, ArchiveSubscription,
    BlockingArchiveHandler, ScalarEventSubscriber, ScalarEventSubscriptionHandler, ScalarHandler,
    ScalarSubscription, ServerContext,
};

use crate::messages::*;
use crate::publish::PublishMode;
use crate::BackupServer;

impl server::ScalarEventHandler<fs::FileSystemEvent> for BackupServer {
    fn handle(
        &mut self,
        msg: fs::FileSystemEvent,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if msg.location == fs::Location::EncryptedRoot && msg.event_type == fs::FileSystemEventType::Mounted {
            log::debug!("encrypted root mounted; loading backup state");
            self.state = Some(FileBacked::new("state.json", fs::Location::AppData).0);
            self.publish_status(self.status(false));
            self.schedule_backup_cb("encrypted-root-mounted");
        }
    }
}

impl server::ArchiveEventHandler<settings::global::OnboardingStatus> for BackupServer {
    fn handle(
        &mut self,
        msg: Owned<settings::global::OnboardingStatus>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.set_onboarding_complete(msg.is_complete());
    }
}

impl server::ScalarEventHandler<settings::global::MagicBackupEnabled> for BackupServer {
    fn handle(
        &mut self,
        msg: settings::global::MagicBackupEnabled,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.set_auto_backup_enabled(msg.0);
    }
}

impl ScalarHandler<PeriodicBackup> for BackupServer {
    fn handle(&mut self, _msg: PeriodicBackup, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.schedule_backup_cb("periodic-callback-fired");
    }
}

impl ArchiveHandler<BackupWorkerEvent> for BackupServer {
    fn handle(
        &mut self,
        event: Owned<BackupWorkerEvent>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        let Ok(event) = event.deserialize() else { return };

        match event {
            BackupWorkerEvent::BackupPublished { published_at, created_at: _ } => {
                log::info!("Backup confirmed published");
                if let Some(state) = self.state.as_mut() {
                    state.guard().last_published_backup = Some(published_at);
                }
                self.publish_status(self.status(false));
            }
            BackupWorkerEvent::BackupPublishFailed => {
                log::warn!("Backup publish failed");
                self.publish_status(self.status(true));
            }
        }
    }
}

impl ScalarEventSubscriptionHandler<StatusSubscribe> for BackupServer {
    fn handle(
        &mut self,
        _msg: StatusSubscribe,
        subscriber: ScalarEventSubscriber<Status>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), <StatusSubscribe as ScalarSubscription>::Error> {
        if subscriber.send(&self.status(false)).is_ok() {
            self.status_subscribers.push(subscriber);
        }

        Ok(())
    }
}

impl BlockingArchiveHandler<CreateBackup> for BackupServer {
    fn handle(
        &mut self,
        _msg: CreateBackup,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <CreateBackup as server::BlockingArchive>::Response {
        self.create_backup(
            crate::BACKUP_FILE,
            crate::BACKUP_LOCATION,
            PublishMode::TimeoutWaitingForEnvoy(crate::BACKUP_NOW_QL_READY_TIMEOUT),
        )
        .map(|_| ())
    }
}

impl BlockingArchiveHandler<CreateBackupFile> for BackupServer {
    fn handle(
        &mut self,
        msg: CreateBackupFile,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <CreateBackupFile as server::BlockingArchive>::Response {
        self.create_backup_internal(&msg.backup_path, msg.location).map(|_| ()).map_err(|e| e.into_inner())
    }
}

impl BlockingArchiveHandler<RestoreBackup> for BackupServer {
    fn handle(
        &mut self,
        msg: RestoreBackup,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), backup::Error> {
        self.restore_backup(&msg.backup_path, msg.location)
    }
}

impl ArchiveEventSubscriptionHandler<SubscribeRestoreProgress> for BackupServer {
    fn handle(
        &mut self,
        _msg: SubscribeRestoreProgress,
        subscriber: ArchiveEventSubscriber<RestoreProgress>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), <SubscribeRestoreProgress as ArchiveSubscription>::Error> {
        self.restore_progress_subscribers.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventHandler<RestoreMagicBackupEvent> for BackupServer {
    fn handle(
        &mut self,
        event: Owned<RestoreMagicBackupEvent>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        match &*event {
            ArchivedRestoreMagicBackupEvent::NotFound => {
                self.notify_restore_sub(RestoreProgress::NotFound);
            }
            ArchivedRestoreMagicBackupEvent::Starting(_) => {
                self.notify_restore_sub(RestoreProgress::Downloading);
            }
            _ => (),
        }

        match self.backup_downloader.handle_event(&*event) {
            Ok(Some(complete)) => {
                log::info!("Magic backup download complete, restoring from {}", complete.file_path);
                let _res = self.restore_backup(&complete.file_path, crate::BACKUP_LOCATION);
            }
            Ok(None) => {
                if let Some((chunks_received, total_chunks)) =
                    self.backup_downloader.get_downloading_progress()
                {
                    log::info!("Restoring backup: {chunks_received}/{total_chunks} chunks");
                }
            }
            Err(e) => match *e {
                crate::downloader::Error::NoBackupFound => {
                    log::info!("no backup found");
                    self.notify_restore_sub(RestoreProgress::NotFound);
                }
                _ => {
                    log::error!("Magic backup restore error: {e:?}");
                    self.notify_restore_sub(RestoreProgress::Error);
                }
            },
        }
    }
}
