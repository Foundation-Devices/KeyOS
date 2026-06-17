// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
pub mod messages;

use std::time::Duration;

use server::{CheckedConn, CheckedPermissions, MessageAllowed};

use crate::messages::*;

#[macro_export]
macro_rules! use_api {
    () => {
        mod rgb_permissions {
            use $crate::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/rgb-server"]
            pub struct RgbPermissions;
        }
        type RgbApi = $crate::RgbApi<rgb_permissions::RgbPermissions>;
    };
}

#[derive(Default)]
pub struct RgbApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

impl<P: CheckedPermissions> RgbApi<P> {
    pub fn try_new_with_timeout(timeout: Duration) -> Option<Self> {
        Some(Self { conn: CheckedConn::try_connect_with_timeout(timeout)? })
    }

    pub fn set_all_to(&self, color: RgbColor)
    where
        P: MessageAllowed<SetAllTo>,
    {
        self.conn.try_send_scalar(SetAllTo(color)).ok();
    }

    pub fn set_to(&self, index: u32, color: RgbColor)
    where
        P: MessageAllowed<SetTo>,
    {
        self.conn.try_send_scalar(SetTo(index, color)).ok();
    }

    pub fn animate_all(&self, animation: RgbAnimation)
    where
        P: MessageAllowed<AnimateAllTo>,
    {
        self.conn.try_send_scalar(AnimateAllTo(animation)).ok();
    }

    #[cfg(not(keyos))]
    pub fn subscribe_color_updates<S>(&self, context: &mut server::ServerContext<S>)
    where
        P: MessageAllowed<messages::SubscribeColorUpdates>,
        S: server::Server + server::ScalarEventHandler<RgbColor>,
    {
        self.conn.subscribe_scalar_infallible(messages::SubscribeColorUpdates, context);
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<u32> for RgbColor {
    fn from(value: u32) -> Self {
        let [r, g, b, _] = value.to_le_bytes();
        Self { r, g, b }
    }
}

impl From<RgbColor> for u32 {
    fn from(value: RgbColor) -> Self {
        let arr = [value.r, value.g, value.b, 0];
        Self::from_le_bytes(arr)
    }
}

impl RgbColor {
    pub const BLACK: RgbColor = RgbColor { r: 0, g: 0, b: 0 };
    pub const RED: RgbColor = RgbColor { r: 0xff, g: 0x00, b: 0x00 };
    pub const TEAL: RgbColor = RgbColor { r: 0x00, g: 0x9d, b: 0xb9 };
    pub const WHITE: RgbColor = RgbColor { r: 0xff, g: 0xff, b: 0xff };

    pub const fn new(r: u8, g: u8, b: u8) -> Self { RgbColor { r, g, b } }

    pub fn lerp(self, other: Self, ratio: f32) -> Self {
        Self {
            r: (self.r as f32 + (other.r as f32 - self.r as f32) * ratio).round() as u8,
            g: (self.g as f32 + (other.g as f32 - self.g as f32) * ratio).round() as u8,
            b: (self.b as f32 + (other.b as f32 - self.b as f32) * ratio).round() as u8,
        }
    }

    pub fn scale(self, ratio: f32) -> Self {
        Self {
            r: (self.r as f32 * ratio).round() as u8,
            g: (self.g as f32 * ratio).round() as u8,
            b: (self.b as f32 * ratio).round() as u8,
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct RgbAnimation {
    pub from: RgbColor,
    pub to: RgbColor,
    pub duration_ms: usize,
    pub reset: bool,
}

impl RgbAnimation {
    pub const fn new(from: RgbColor, to: RgbColor, duration_ms: usize, reset: bool) -> Self {
        Self { from, to, duration_ms, reset }
    }
}
