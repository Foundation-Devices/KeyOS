// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::SystemTime;

use server::rkyv_with::WithUnixTimestamp;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct PeriodicBackup;

#[derive(Debug, Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum BackupWorkerEvent {
    BackupPublished {
        #[rkyv(with = WithUnixTimestamp)]
        created_at: SystemTime,
        #[rkyv(with = WithUnixTimestamp)]
        published_at: SystemTime,
    },
    BackupPublishFailed,
}
