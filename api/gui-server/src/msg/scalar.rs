// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use num_traits::{FromPrimitive, ToPrimitive};
use server::{AsScalar, FromScalar};

use crate::NextFrameAnimationKind;

#[derive(Debug, server::Message)]
pub struct SwitchTo {
    pub next_pid: usize,
    pub x: usize,
    pub y: usize,
}

impl AsScalar<3> for SwitchTo {
    fn as_scalar(&self) -> [u32; 3] { [self.next_pid as u32, self.x as u32, self.y as u32] }
}

impl FromScalar<3> for SwitchTo {
    fn from_scalar([pid, x, y]: [u32; 3]) -> Self {
        Self { next_pid: pid as usize, x: x as usize, y: y as usize }
    }
}

#[derive(Debug, server::Message)]
pub struct RequestRedraw;

#[derive(
    Debug,
    PartialEq,
    num_derive::FromPrimitive,
    num_derive::ToPrimitive,
    Copy,
    Clone,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub enum AppTheme {
    System,
    Dark,
    Light,
}

impl server::AsScalar<1> for AppTheme {
    fn as_scalar(&self) -> [u32; 1] { [self.to_u32().unwrap_or(0)] }
}

impl server::FromScalar<1> for AppTheme {
    fn from_scalar([value]: [u32; 1]) -> Self { Self::from_u32(value).unwrap_or(AppTheme::System) }
}

#[derive(Debug, server::Message)]
#[response(())]
pub struct Shutdown {
    pub reboot: bool,
}

impl FromScalar<1> for Shutdown {
    fn from_scalar(value: [u32; 1]) -> Self { Self { reboot: bool::from_scalar(value) } }
}

impl AsScalar<1> for Shutdown {
    fn as_scalar(&self) -> [u32; 1] { bool::as_scalar(&self.reboot) }
}

#[derive(Debug, server::Message)]
#[response(bool)]
pub struct SwitchToLauncher;

#[derive(Debug, server::Message)]
pub struct CloseApp {
    pub pid: usize,
}

impl FromScalar<1> for CloseApp {
    fn from_scalar([pid]: [u32; 1]) -> Self { Self { pid: pid as usize } }
}

impl AsScalar<1> for CloseApp {
    fn as_scalar(&self) -> [u32; 1] { [self.pid as u32] }
}

#[derive(Debug, server::Message)]
pub struct AnimateNextFrame {
    pub animation_kind: NextFrameAnimationKind,
}

/// Prevents the device from auto-locking and auto-shutting down while active
/// Screen dimming is still allowed
#[derive(Debug, Copy, Clone, server::Message)]
pub struct SetWakeLock(pub bool);

impl FromScalar<1> for AnimateNextFrame {
    fn from_scalar([animation_kind]: [u32; 1]) -> Self {
        Self { animation_kind: NextFrameAnimationKind::from_u32(animation_kind).unwrap_or_default() }
    }
}

impl AsScalar<1> for AnimateNextFrame {
    fn as_scalar(&self) -> [u32; 1] { [self.animation_kind as u32] }
}
