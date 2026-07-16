// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod error;
pub mod messages;

use std::time::{Duration, SystemTime};

use server::{AsScalar, CheckedConn, CheckedPermissions, FromScalar, MessageAllowed, Server, ServerContext};

pub use crate::error::Error;
use crate::messages::*;

/// folder that will be ignored when creating backups
pub const DO_NOT_BACKUP_FOLDER: &str = "no_backup";

#[macro_export]
macro_rules! use_api {
    () => {
        mod backup_permissions {
            use backup::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/backup"]
            pub struct BackupPermissions;
        }
        type BackupApi = backup::BackupApi<backup_permissions::BackupPermissions>;
    };
}

#[derive(Default)]
pub struct BackupApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

impl<P: CheckedPermissions> BackupApi<P> {
    pub fn create_backup(&self) -> Result<(), Error>
    where
        P: MessageAllowed<CreateBackup>,
    {
        self.conn.send_blocking_archive(CreateBackup)
    }

    pub fn create_backup_file(&self, backup_path: String, location: fs::Location) -> Result<(), Error>
    where
        P: MessageAllowed<CreateBackupFile>,
    {
        self.conn.send_blocking_archive(CreateBackupFile { backup_path, location })
    }

    pub fn restore_backup(&self, backup_path: String, location: fs::Location) -> Result<(), Error>
    where
        P: MessageAllowed<RestoreBackup>,
    {
        self.conn.send_blocking_archive(RestoreBackup { backup_path, location })
    }

    pub fn subscribe_restore_progress<S>(&self, context: &mut ServerContext<S>)
    where
        P: MessageAllowed<SubscribeRestoreProgress>,
        S: Server + server::ArchiveEventHandler<RestoreProgress>,
    {
        self.conn.subscribe_archive_infallible(SubscribeRestoreProgress, context);
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct Status {
    pub last_backup_at: Option<SystemTime>,
    pub publish_failed: bool,
}

impl FromScalar<3> for Status {
    fn from_scalar([last_backed_up_at_h, last_backed_up_at_l, publish_failed]: [u32; 3]) -> Self {
        let last_backup_at = if last_backed_up_at_h != 0 || last_backed_up_at_l != 0 {
            let h = last_backed_up_at_h.to_le_bytes();
            let l = last_backed_up_at_l.to_le_bytes();
            let last_backed_up_at_u64 = u64::from_le_bytes([h[0], h[1], h[2], h[3], l[0], l[1], l[2], l[3]]);
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(last_backed_up_at_u64))
        } else {
            None
        };

        Status { last_backup_at, publish_failed: publish_failed != 0 }
    }
}

impl AsScalar<3> for Status {
    fn as_scalar(&self) -> [u32; 3] {
        let (last_backed_up_at_h, last_backed_up_at_l) = if let Some(last_backup_at) = self.last_backup_at {
            let [h1, h2, h3, h4, l1, l2, l3, l4] =
                last_backup_at.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs().to_le_bytes();
            (u32::from_le_bytes([h1, h2, h3, h4]), u32::from_le_bytes([l1, l2, l3, l4]))
        } else {
            (0, 0)
        };

        [last_backed_up_at_h, last_backed_up_at_l, u32::from(self.publish_failed)]
    }
}
