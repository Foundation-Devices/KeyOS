// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod core;
mod downloader;
mod handlers;
mod messages;
mod publish;
mod utils;

use core::{BackupFile, BackupKey, BackupMetadata};
use std::time::{Duration, SystemTime};

use backup::{messages::*, Status};
use downloader::BackupDownloader;
use file_backed::JsonBacked;
use quantum_link::{
    foundation_api::backup::RestoreMagicBackupResult, messages::SendRestoreMagicBackupResult,
};
use server::xous;
use server::{
    AllPermissions, ArchiveEventSubscriber, CheckedConn, MessageId as _, ScalarEventSubscriber, Server,
    ServerContext,
};
use whence::WhenceExt;
use worker::{TaskHandle, WorkerHandle};
use xous_ticktimer::TicktimerCallback;

use crate::fs_permissions::FileSystemPermissions;
use crate::messages::*;

quantum_link::use_api!();
fs::use_api!();
crypto::use_api!();
security::use_api!();
settings::use_api!();

const BACKUP_INTERVAL: Duration = Duration::from_secs(60 * 60 * 12);
const BACKUP_NOW_QL_READY_TIMEOUT: Duration = Duration::from_secs(90);
const BACKUP_FILE: &str = "backup/backup.tar";
const BACKUP_LOCATION: fs::Location = fs::Location::EncryptedRoot;

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    // TODO: choose thread priority
    // xous::set_thread_priority(xous::ThreadPriority::System7).unwrap();

    // QL is not started until we are logged in
    // block on QL BEFORE creating backup server and registering with XousNames
    let ql = QuantumLinkApi::default();
    server::listen_with(|sid| BackupServer::new(sid, ql))
}

#[derive(server::Server)]
#[name = "os/backup"]
pub struct BackupServer {
    #[allow(unused)]
    sid: xous::SID,
    fs: FileSystem,
    ql: QuantumLinkApi,
    security: Security,
    settings: SettingsApi,

    status_subscribers: Vec<ScalarEventSubscriber<Status>>,
    restore_progress_subscribers: Vec<ArchiveEventSubscriber<RestoreProgress>>,

    // not available until we mount encrypted partition
    state: Option<JsonBacked<BackupState, FileSystemPermissions>>,
    // store backup key, so login state doesn't obstruct backups
    backup_key: Option<BackupKey>,

    backup_cb: TicktimerCallback,
    backup_downloader: BackupDownloader<FileSystem>,

    auto_backup_enabled: bool,
    onboarding_complete: bool,

    worker: WorkerHandle,
    ql_status: QlStatus,
    server_sender: BackupServerSender,
    publish_backup_task: Option<TaskHandle<()>>,
    notify_backup_result: Option<TaskHandle<()>>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct BackupState {
    last_created_backup: Option<SystemTime>,
    last_published_backup: Option<SystemTime>,
}

#[derive(Clone)]
pub struct BackupServerSender {
    conn: CheckedConn<AllPermissions>,
}

impl BackupServerSender {
    pub fn send(&self, event: BackupWorkerEvent) { self.conn.try_send_archive(event).ok(); }
}

impl Server for BackupServer {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        let settings = SettingsApi::default();
        settings.server_subscribe_magic_backup_enabled(context);
        settings.server_subscribe_onboarding_status(context);
        self.ql.subscribe_restore_magic_backup(context);
        self.fs.subscribe_filesystem_events(context, fs::Location::EncryptedRoot);
    }
}

impl BackupServer {
    pub fn new(sid: xous::SID, ql: QuantumLinkApi) -> Self {
        let fs = FileSystem::default();
        let backup_downloader = BackupDownloader::new(fs.clone().into());
        let backup_cb = TicktimerCallback::new(sid).unwrap();
        let worker = WorkerHandle::default();
        let ql_status = QlStatus::new(worker.clone());
        Self {
            sid,
            fs,
            ql,
            security: Default::default(),
            settings: Default::default(),

            status_subscribers: Vec::new(),
            restore_progress_subscribers: Vec::new(),

            state: None,
            backup_key: None,
            backup_cb,
            backup_downloader,
            auto_backup_enabled: true,
            onboarding_complete: false,

            ql_status,
            worker,
            server_sender: BackupServerSender { conn: xous::connect(sid).unwrap().into() },
            publish_backup_task: None,
            notify_backup_result: None,
        }
    }

    fn status(&self, publish_failed: bool) -> Status {
        let last_backup_at = self.state.as_ref().and_then(|state| state.last_published_backup);
        Status { last_backup_at, publish_failed }
    }

    fn publish_status(&mut self, status: Status) {
        self.status_subscribers.retain(|s| s.send(&status).is_ok());
    }

    fn get_backup_key(&mut self) -> Result<BackupKey, backup::Error> {
        match self.backup_key.as_ref().cloned() {
            Some(key) => Ok(key),
            None => {
                let app_seed = self.security.app_seed()?;
                Ok(self.backup_key.insert(BackupKey::from_app_seed(app_seed)).clone())
            }
        }
    }

    fn create_backup(
        &mut self,
        backup_path: &str,
        backup_location: fs::Location,
        publish_mode: publish::PublishMode,
    ) -> Result<BackupFile, backup::Error> {
        let res = self.create_backup_internal(backup_path, backup_location).map_err(|e| {
            log::info!("create backup failed: {e:?}");
            e.into_inner()
        });

        if let Ok(backup_file) = &res {
            if let Some(state) = self.state.as_mut() {
                state.guard().last_created_backup = Some(backup_file.created_at);
            }

            let sender = self.server_sender.clone();
            let worker = self.worker.clone();
            let ql_status = self.ql_status.clone();
            let backup_file = backup_file.clone();

            let task = async move {
                let res = publish::publish_backup(&worker, &ql_status, &backup_file, publish_mode).await;
                match res {
                    Ok(_) => sender.send(BackupWorkerEvent::BackupPublished {
                        created_at: backup_file.created_at,
                        published_at: SystemTime::now(),
                    }),
                    Err(e) => {
                        log::error!("Failed to publish backup: {e:?}");
                        sender.send(BackupWorkerEvent::BackupPublishFailed);
                    }
                }
            };
            self.publish_backup_task = Some(self.worker.spawn(task));
        }

        res
    }

    fn create_backup_internal(
        &mut self,
        backup_path: &str,
        backup_location: fs::Location,
    ) -> whence::Result<BackupFile, backup::Error> {
        let backup_key = self.get_backup_key().whence()?;
        let device_name = Some(self.settings.get_device_name().0);
        core::create_backup::<_, core::v2::CryptoLive>(
            &self.fs,
            backup_path,
            backup_location,
            &backup_key,
            device_name,
        )
    }

    fn restore_backup(&mut self, backup_path: &str, location: fs::Location) -> Result<(), backup::Error> {
        self.notify_restore_sub(RestoreProgress::Restoring);
        let result = self.restore_backup_internal(backup_path, location).map_err(|e| {
            log::error!("restore backup failed: {e:?}");
            e.into_inner()
        });

        match &result {
            Ok(metadata) => {
                self.notify_restore_sub(RestoreProgress::Restored);
                // update backup state with the restored backup's creation time
                if let Some(state) = self.state.as_mut() {
                    let mut state = state.guard();
                    state.last_created_backup = Some(metadata.created_at);
                    state.last_published_backup = Some(metadata.created_at);
                }
                if let Some(name) = metadata.device_name.clone() {
                    self.settings.set_device_name(settings::global::DeviceName(name));
                }
            }
            Err(_) => {
                self.notify_restore_sub(RestoreProgress::Error);
            }
        }

        self.notify_backup_result = {
            let result = match &result {
                Ok(_) => RestoreMagicBackupResult::Success,
                Err(e) => RestoreMagicBackupResult::Error { error: e.to_string() },
            };
            let task = notify_restore_result(self.ql_status.clone(), result);
            Some(self.worker.spawn(task))
        };
        result.map(|_| ())
    }

    fn restore_backup_internal(
        &mut self,
        backup_path: &str,
        backup_location: fs::Location,
    ) -> whence::Result<BackupMetadata, backup::Error> {
        let backup_key = self.get_backup_key().whence()?;
        core::restore_backup::<_, core::v1::CryptoLive, core::v2::CryptoLive>(
            &self.fs,
            backup_path,
            backup_location,
            &backup_key,
        )
    }

    fn set_onboarding_complete(&mut self, onboarding_complete: bool) {
        self.onboarding_complete = onboarding_complete;
        self.schedule_backup_cb("onboarding-status");
    }

    fn set_auto_backup_enabled(&mut self, enabled: bool) {
        self.auto_backup_enabled = enabled;
        self.schedule_backup_cb("magic-backup-enabled-setting");
    }

    fn schedule_backup_cb(&mut self, reason: &str) {
        log::debug!(
            "[auto-backup] schedule requested: reason={reason}, enabled={}, onboarding_complete={}, state_ready={}",
            self.auto_backup_enabled,
            self.onboarding_complete,
            self.state.is_some(),
        );

        if !self.auto_backup_enabled || !self.onboarding_complete {
            log::debug!("[auto-backup] schedule skipped: prerequisites not met");
            return;
        }

        let Some(last_created_backup) = self.state.as_ref().map(|state| state.last_created_backup) else {
            log::debug!("[auto-backup] schedule skipped: backup state unavailable");
            return;
        };

        let delay = match last_created_backup {
            Some(last_created_backup) => match SystemTime::now().duration_since(last_created_backup) {
                Ok(elapsed) => {
                    let remaining = BACKUP_INTERVAL.checked_sub(elapsed).unwrap_or_default();
                    log::debug!(
                        "[auto-backup] schedule computed from last_created_backup={last_created_backup:?}, elapsed={}s, delay={}s",
                        elapsed.as_secs(),
                        remaining.as_secs(),
                    );
                    remaining
                }
                Err(e) => {
                    log::debug!(
                        "[auto-backup] schedule clock error (last backup in future by {}s), running now",
                        e.duration().as_secs()
                    );
                    Duration::ZERO
                }
            },
            None => {
                log::debug!("[auto-backup] schedule has no previous backup, running now");
                Duration::ZERO
            }
        };

        if delay.is_zero() {
            self.create_backup(BACKUP_FILE, BACKUP_LOCATION, publish::PublishMode::WaitForEnvoy).ok();
            self.request_backup_cb(BACKUP_INTERVAL, "interval-elapsed");
        } else {
            self.request_backup_cb(delay, "waiting-for-next-interval");
        }
    }

    fn request_backup_cb(&mut self, delay: Duration, reason: &str) {
        log::debug!("[auto-backup] requesting backup callback in {}s: reason={reason}", delay.as_secs());
        self.backup_cb.request(delay.as_millis() as usize, PeriodicBackup::ID, 0);
    }

    fn notify_restore_sub(&mut self, event: RestoreProgress) {
        self.restore_progress_subscribers.retain(|s| s.send(&event).is_ok());
    }
}

async fn notify_restore_result(ql_status: QlStatus, result: RestoreMagicBackupResult) {
    ql_status
        .send_ql_archive_retry(SendRestoreMagicBackupResult { result }, |e| {
            log::warn!("failed to publish RestoreMagicBackupResult {e:?}")
        })
        .await;
    log::info!("notified magic backup restore result");
}
