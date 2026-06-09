// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod error;
pub mod messages;

pub use error::{DownloadError, Error};
pub use messages::UpdateStatus;

/// Minimum battery percentage required for firmware updates.
pub const MIN_UPDATE_BATTERY_PERCENT: u8 = 20;

#[macro_export]
macro_rules! use_api {
    () => {
        mod update_permissions {
            use update::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/update"]
            pub struct UpdatePermissions;
        }
        type UpdateApi = update::UpdateApi<update_permissions::UpdatePermissions>;
    };
}

#[derive(Clone, Debug, Default)]
pub struct UpdateApi<P: server::CheckedPermissions> {
    conn: server::CheckedConn<P>,
}

impl<P: server::CheckedPermissions> UpdateApi<P> {
    pub fn subscribe_update<S>(&self, context: &mut server::ServerContext<S>)
    where
        S: server::Server + server::ArchiveEventHandler<messages::ProgressUpdate>,
        P: server::MessageAllowed<messages::SubscribeUpdateProgress>,
    {
        self.conn.subscribe_archive_infallible(messages::SubscribeUpdateProgress, context)
    }

    /// Apply a series of successive updates to the device, sending progress/error updates to the
    /// caller.
    ///
    /// If any of the releases require a reboot, the update server with store the information about
    /// the remaining releases to be applied and initiate a reboot.
    ///
    /// To check whether an update has been stopped midway by a reboot, check
    /// [UpdateStatus::needs_continue]. To resume an update that was stopped midway by a reboot,
    /// call [Self::continue_update].
    ///
    /// NOTE: Paths to the release files **must be absolute**.
    pub fn start_update(&self, release_paths: Vec<String>)
    where
        P: server::MessageAllowed<messages::StartUpdate>,
    {
        self.conn.send_archive(messages::StartUpdate { release_paths })
    }

    /// Continue an update that was interrupted by a reboot. Make sure that this is called only if
    /// [UpdateStatus::needs_continue] is true.
    pub fn continue_update(&self)
    where
        P: server::MessageAllowed<messages::ContinueUpdate>,
    {
        self.conn.send_archive(messages::ContinueUpdate)
    }

    /// Get the current firmware version.
    pub fn firmware_version(&self) -> Result<String, Error>
    where
        P: server::MessageAllowed<messages::FirmwareVersion>,
    {
        self.conn.send_blocking_archive(messages::FirmwareVersion)
    }

    /// Apply the previously downloaded firmware update. This should be called after receiving a
    /// [`messages::ProgressUpdate::DownloadComplete`] event.
    pub fn apply_downloaded_update(&self)
    where
        P: server::MessageAllowed<messages::ApplyDownloadedUpdate>,
    {
        self.conn.send_archive(messages::ApplyDownloadedUpdate)
    }

    /// Check whether an update was applied and is awaiting acknowledgment after reboot.
    pub fn check_update_applied(&self) -> bool
    where
        P: server::MessageAllowed<messages::GetUpdateApplied>,
    {
        self.conn.send_blocking_scalar(messages::GetUpdateApplied)
    }

    /// Clear the update applied flag after acknowledging the update on reboot.
    /// Note: The flag is automatically set to true by the update server when applying an update.
    pub fn clear_update_applied(&self)
    where
        P: server::MessageAllowed<messages::ClearUpdateApplied>,
    {
        self.conn.send_scalar(messages::ClearUpdateApplied)
    }

    /// Get the current update status, including whether an update is available and if battery
    /// is sufficient.
    pub fn update_status(&self) -> messages::UpdateStatus
    where
        P: server::MessageAllowed<messages::GetUpdateStatus>,
    {
        self.conn.send_blocking_archive(messages::GetUpdateStatus)
    }
}
