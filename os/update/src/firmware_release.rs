// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use update::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FirmwareRelease {
    timestamp: u32,
    is_pre_release: bool,
}

impl FirmwareRelease {
    pub(crate) fn timestamp_to_persist(self) -> Option<u32> {
        (!self.is_pre_release).then_some(self.timestamp)
    }
}

#[derive(Debug)]
pub(crate) struct FirmwareReleaseTracker {
    min_allowed_timestamp: u32,
    last_release: Option<FirmwareRelease>,
    last_stable_timestamp: Option<u32>,
}

impl FirmwareReleaseTracker {
    pub(crate) fn new(min_allowed_timestamp: u32) -> Self {
        Self { min_allowed_timestamp, last_release: None, last_stable_timestamp: None }
    }

    /// Records a cryptographically verified release while enforcing the rollback floor
    pub(crate) fn record_verified(&mut self, version: &str, timestamp: u32) -> Result<(), Error> {
        if timestamp < self.min_allowed_timestamp {
            return Err(Error::RollbackPrevented { current: self.min_allowed_timestamp, update: timestamp });
        }

        let release = FirmwareRelease {
            timestamp,
            is_pre_release: version.contains("alpha") || version.contains("beta"),
        };

        self.min_allowed_timestamp = timestamp;
        if let Some(timestamp) = release.timestamp_to_persist() {
            self.last_stable_timestamp = Some(timestamp);
        }
        self.last_release = Some(release);
        Ok(())
    }

    pub(crate) fn last_release(&self) -> Option<FirmwareRelease> { self.last_release }

    pub(crate) fn timestamp_to_persist(&self) -> Option<u32> { self.last_stable_timestamp }
}
