// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use update::Error;

#[derive(Debug)]
pub(crate) struct FirmwareReleaseTracker {
    min_allowed_timestamp: u32,
}

impl FirmwareReleaseTracker {
    pub(crate) fn new(min_allowed_timestamp: u32) -> Self { Self { min_allowed_timestamp } }

    /// Records a cryptographically verified release while enforcing the rollback floor
    pub(crate) fn record_verified(&mut self, version: &str, timestamp: u32) -> Result<Option<u32>, Error> {
        if timestamp < self.min_allowed_timestamp {
            return Err(Error::RollbackPrevented { current: self.min_allowed_timestamp, update: timestamp });
        }

        self.min_allowed_timestamp = timestamp;
        Ok((!version.contains("alpha") && !version.contains("beta")).then_some(timestamp))
    }
}
