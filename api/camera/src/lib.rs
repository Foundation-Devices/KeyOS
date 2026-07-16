// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(keyos))]
mod hosted;
pub mod messages;

use std::time::Duration;

#[cfg(not(keyos))]
pub use hosted::*;
use server::{CheckedConn, CheckedPermissions, MessageAllowed};

use crate::messages::*;

// Center the camera vertically with the UI's viewfinder image
pub const CAMERA_WIDTH: usize = 480;
pub const CAMERA_HEIGHT: usize = 480;

// Duplicating this constant here to avoid an unnecessary dependency on gui-server-api
const SCREEN_HEIGHT: usize = 800;

// Margin on the top and bottom of the camera framebuffer so we can crop a SCREEN_HEIGHT
// slice out of it at any vertical offset.
pub const CAMERA_MARGIN: usize = SCREEN_HEIGHT - CAMERA_HEIGHT;

#[cfg(keyos)]
pub const CAMERA_BYTES_PER_PX: usize = 2; // RGB565 (2 bytes) is used on hardware
#[cfg(not(keyos))]
pub const CAMERA_BYTES_PER_PX: usize = 4;
pub const CAMERA_FB_SIZE_BYTES: usize =
    CAMERA_WIDTH * (CAMERA_HEIGHT + CAMERA_MARGIN * 2) * CAMERA_BYTES_PER_PX;

pub const SERVER_NAME: &str = "os/camera";

#[macro_export]
macro_rules! use_api {
    () => {
        mod camera_permissions {
            use camera::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/camera"]
            pub struct CameraPermissions;
        }
        type CameraApi = camera::CameraApi<camera_permissions::CameraPermissions>;
    };
}

#[derive(Default)]
pub struct CameraApi<P: CheckedPermissions>(CheckedConn<P>);

impl<P: CheckedPermissions> CameraApi<P> {
    pub fn try_new_with_timeout(timeout: Duration) -> Option<Self> {
        CheckedConn::try_connect_with_timeout(timeout).map(Self)
    }

    /// Start requesting camera frames. Cheap because the frames are not actually sent,
    /// but mirrored on the client side. The event is sent when a new frame is available.
    pub fn subscribe<S>(&self, context: &mut server::ServerContext<S>) -> Result<(), SubscriptionError>
    where
        S: server::Server + server::ScalarEventHandler<Frame>,
        P: MessageAllowed<Subscribe>,
    {
        self.0.subscribe_scalar(Subscribe, context)
    }

    /// Enable the use of the camera. Intended to be used by the control center
    pub fn set_enabled(&self, enabled: bool)
    where
        P: MessageAllowed<SetEnabled>,
    {
        self.0.send_scalar(SetEnabled(enabled));
    }

    /// Notify the app that the camera image is visible on the screen.
    /// Intended to be used by the GUI server
    pub fn notify_visible(&self, visible: bool)
    where
        P: MessageAllowed<NotifyVisible>,
    {
        self.0.send_scalar(NotifyVisible(visible));
    }

    pub fn is_enabled(&self) -> bool
    where
        P: MessageAllowed<IsEnabled>,
    {
        self.0.send_blocking_scalar(IsEnabled)
    }

    pub fn is_in_use(&self) -> bool
    where
        P: MessageAllowed<IsInUse>,
    {
        self.0.send_blocking_scalar(IsInUse)
    }

    /// Get current camera parameters
    pub fn get_params(&self) -> Result<CameraParams, xous::Error>
    where
        P: MessageAllowed<GetParams>,
    {
        self.0.try_send_blocking_archive(GetParams).map_err(From::from)
    }

    /// Set camera parameters
    pub fn set_params(&self, params: CameraParams) -> Result<(), xous::Error>
    where
        P: MessageAllowed<SetParams>,
    {
        self.0.try_send_archive(SetParams(params))?;
        Ok(())
    }
}

#[cfg(keyos)]
pub struct Frame(xous::MemoryRange);
#[cfg(keyos)]
server::wrapped_scalar!(Frame);

#[cfg(keyos)]
impl Frame {
    pub fn new(range: xous::MemoryRange) -> Self { Self(range) }

    pub fn padded_range(&self) -> xous::MemoryRange { self.0 }

    pub fn content(&self) -> xous::MemoryRange {
        self.0
            .subrange(
                CAMERA_WIDTH * CAMERA_MARGIN * CAMERA_BYTES_PER_PX,
                CAMERA_WIDTH * CAMERA_HEIGHT * CAMERA_BYTES_PER_PX,
            )
            .unwrap()
    }
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum SubscriptionError {
    OutOfMemory,
    Other,
}
