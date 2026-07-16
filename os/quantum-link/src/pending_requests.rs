// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::{Duration, Instant};

use quantum_link::{foundation_api::backup::CreateMagicBackupResult, messages::*, SendMessageError};
use server::ArchiveRequest;

#[derive(Debug, Default)]
pub struct PendingRequests {
    pub envoy_magic_backup_enabled: Option<PendingRequest<EnvoyMagicBackupEnabled>>,
    pub update_check: Option<PendingRequest<CheckFirmwareUpdate>>,
    pub update_start: Option<PendingRequest<StartFirmwareUpdate>>,
    pub backup_shard: Option<PendingRequest<BackupShard>>,
    pub restore_shard: Option<PendingRequest<RestoreShard>>,
    pub restore_magic_backup: Option<PendingRequest<StartRestoreMagicBackup>>,
    pub prime_magic_backup_status_response: Option<PendingRequest<MagicBackupStatus>>,
    pub timezone: Option<PendingRequest<EnvoyTimezone>>,

    // does not have an initiator
    pub create_magic_backup_result: Option<PendingRequest<AwaitCreateMagicBackupResult>>,
}

impl PendingRequests {
    /// Force every pending request to expire immediately and fire its timeout
    /// response on the next `cleanup_expired`. Used when BLE drops, since
    /// heartbeat ticks stop while disconnected and the periodic cleanup path
    /// would never reach the natural expiration.
    pub fn expire_all_now(&mut self) {
        fn expire<T: server::BlockingArchive>(opt: &mut Option<PendingRequest<T>>) {
            if let Some(req) = opt.as_mut() {
                req.expiration = Instant::now();
            }
        }
        expire(&mut self.envoy_magic_backup_enabled);
        expire(&mut self.update_check);
        expire(&mut self.update_start);
        expire(&mut self.backup_shard);
        expire(&mut self.restore_shard);
        expire(&mut self.restore_magic_backup);
        expire(&mut self.prime_magic_backup_status_response);
        expire(&mut self.timezone);
        expire(&mut self.create_magic_backup_result);
    }

    pub fn cleanup_expired(&mut self) {
        fn cleanup<T>(opt: &mut Option<PendingRequest<T>>, now: Instant)
        where
            T: server::BlockingArchive,
            T::Response: TimeoutResponse,
        {
            if let Some(req) = opt.take_if(|req| req.is_expired(now)) {
                log::info!("request timed out {}", type_name::<T>());
                req.respond(T::Response::timeout());
            }
        }

        let now = Instant::now();
        cleanup(&mut self.envoy_magic_backup_enabled, now);
        cleanup(&mut self.update_check, now);
        cleanup(&mut self.update_start, now);
        cleanup(&mut self.backup_shard, now);
        cleanup(&mut self.restore_shard, now);
        cleanup(&mut self.restore_magic_backup, now);
        cleanup(&mut self.prime_magic_backup_status_response, now);
        cleanup(&mut self.timezone, now);
        cleanup(&mut self.create_magic_backup_result, now);
    }
}

pub struct PendingRequest<T: server::BlockingArchive> {
    inner: ArchiveRequest<T>,
    expiration: Instant,
}

impl<T> std::fmt::Debug for PendingRequest<T>
where
    T: server::BlockingArchive + std::fmt::Debug,
    T::Response: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingRequest")
            .field("inner", &self.inner)
            .field("expiration", &self.expiration)
            .finish()
    }
}

impl<T: server::BlockingArchive> PendingRequest<T> {
    pub fn new(request: ArchiveRequest<T>) -> Self
    where
        T: TimeoutDuration,
    {
        Self { inner: request, expiration: Instant::now() + T::TIMEOUT }
    }

    pub fn is_expired(&self, now: Instant) -> bool { now >= self.expiration }

    pub fn respond(self, response: T::Response) {
        if let Err(e) = self.inner.response.respond(response) {
            log::warn!("failed to respond to {} {e}", type_name::<T>());
        }
    }
}

#[derive(Debug, Copy, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum PendingRequestKind {
    EnvoyMagicBackupEnabled,
    CheckFirmwareUpdate,
    StartFirmwareUpdate,
    BackupShard,
    RestoreShard,
    RestoreMagicBackup,
    MagicBackupStatus,
    EnvoyTimezone,
}

pub fn type_name<T>() -> &'static str {
    let name = std::any::type_name::<T>();
    name.rsplit_once("::").map(|(_, r)| r).unwrap_or(name)
}

trait TimeoutResponse {
    fn timeout() -> Self;
}

impl<T> TimeoutResponse for Result<T, SendMessageError> {
    fn timeout() -> Self { Err(SendMessageError::Timeout) }
}

impl TimeoutResponse for CreateMagicBackupResult {
    fn timeout() -> Self { CreateMagicBackupResult::Error { error: String::from("timeout") } }
}

pub trait TimeoutDuration {
    const TIMEOUT: Duration = Duration::from_secs(4);
}

impl TimeoutDuration for CheckFirmwareUpdate {
    const TIMEOUT: Duration = Duration::from_secs(8);
}

impl TimeoutDuration for StartFirmwareUpdate {
    const TIMEOUT: Duration = Duration::from_secs(8);
}

impl TimeoutDuration for BackupShard {}
impl TimeoutDuration for RestoreShard {}
impl TimeoutDuration for EnvoyMagicBackupEnabled {}
impl TimeoutDuration for MagicBackupStatus {}
impl TimeoutDuration for StartRestoreMagicBackup {}
impl TimeoutDuration for EnvoyTimezone {}

// Envoy must upload to cloud + ack over BLE; 90s covers slow uploads
// before we give up and tell the user.
impl TimeoutDuration for AwaitCreateMagicBackupResult {
    const TIMEOUT: Duration = Duration::from_secs(90);
}

#[test]
fn ty_name() {
    assert_eq!(type_name::<MagicBackupStatus>(), "MagicBackupStatus");
}
