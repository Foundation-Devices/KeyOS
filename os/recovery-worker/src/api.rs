// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use fs::Location;
use server::{CheckedPermissions, MessageAllowed};

use crate::error::RecoveryWorkerError;
use crate::messages::*;

#[derive(Default)]
pub struct RecoveryWorkerApi<P: CheckedPermissions>(pub(crate) server::CheckedConn<P>);

impl<P: CheckedPermissions> RecoveryWorkerApi<P> {
    pub fn read_recovery_archive(&self, path: &str, location: Location)
    where
        P: MessageAllowed<ReadArchive>,
    {
        self.0.send_blocking_archive(ReadArchive { path: path.to_string(), location })
    }

    pub fn archive_state(&self) -> ArchiveState
    where
        P: MessageAllowed<GetArchiveState>,
    {
        self.0.send_blocking_archive(GetArchiveState)
    }

    pub fn app_bin_verification_state(&self) -> AppBinVerificationState
    where
        P: MessageAllowed<GetAppBinVerificationState>,
    {
        self.0.send_blocking_archive(GetAppBinVerificationState)
    }

    pub fn copy_archive(&self) -> Result<(), RecoveryWorkerError>
    where
        P: MessageAllowed<CopyArchive>,
    {
        self.0.try_send_scalar(CopyArchive)?;
        Ok(())
    }

    pub fn start_recovery(&self) -> Result<(), RecoveryWorkerError>
    where
        P: MessageAllowed<StartRecovery>,
    {
        self.0.try_send_scalar(StartRecovery)?;
        Ok(())
    }

    pub fn last_error(&self) -> Option<String>
    where
        P: MessageAllowed<GetLastError>,
    {
        self.0.send_blocking_archive(GetLastError)
    }
}
