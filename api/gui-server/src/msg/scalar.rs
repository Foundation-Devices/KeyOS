// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use num_traits::{FromPrimitive, ToPrimitive};
use server::{AsScalar, FromScalar};

use crate::{ControlCenterColor, GuiServerError, NextFrameAnimationKind};

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

/// Sets the background color of the collapsed Control Center while the sending
/// app is visible.
/// `None` restores the system theme color.
#[derive(Debug, Copy, Clone, server::Message)]
pub struct SetControlCenterColor {
    pub color: Option<ControlCenterColor>,
}

impl AsScalar<1> for SetControlCenterColor {
    fn as_scalar(&self) -> [u32; 1] {
        let packed = self.color.map_or(0, |color| {
            (1 << 24) | ((color.red as u32) << 16) | ((color.green as u32) << 8) | color.blue as u32
        });
        [packed]
    }
}

impl FromScalar<1> for SetControlCenterColor {
    fn from_scalar([packed]: [u32; 1]) -> Self {
        let color = (packed & (1 << 24) != 0).then(|| {
            ControlCenterColor::new(
                ((packed >> 16) & 0xff) as u8,
                ((packed >> 8) & 0xff) as u8,
                (packed & 0xff) as u8,
            )
        });
        Self { color }
    }
}

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
#[response(bool)]
pub struct IsLocked;

#[derive(Debug, server::Message)]
#[response(Result<(), GuiServerError>)]
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

#[derive(Debug, Copy, Clone, Default, server::Message)]
pub struct UpdateKioskPolicy {
    pub auto_lock_enabled: Option<bool>,
    pub control_center_enabled: Option<bool>,
    pub home_button_enabled: Option<bool>,
    pub power_button_enabled: Option<bool>,
}

impl UpdateKioskPolicy {
    const AUTO_LOCK: u32 = 1 << 0;
    const CONTROL_CENTER: u32 = 1 << 1;
    const HOME_BUTTON: u32 = 1 << 2;
    const POWER_BUTTON: u32 = 1 << 3;

    pub fn all(enabled: bool) -> Self {
        Self::default()
            .set_auto_lock(enabled)
            .set_control_center(enabled)
            .set_home_button(enabled)
            .set_power_button(enabled)
    }

    #[inline]
    pub fn set_auto_lock(mut self, enabled: bool) -> Self {
        self.auto_lock_enabled = Some(enabled);
        self
    }

    #[inline]
    pub fn set_control_center(mut self, enabled: bool) -> Self {
        self.control_center_enabled = Some(enabled);
        self
    }

    #[inline]
    pub fn set_home_button(mut self, enabled: bool) -> Self {
        self.home_button_enabled = Some(enabled);
        self
    }

    #[inline]
    pub fn set_power_button(mut self, enabled: bool) -> Self {
        self.power_button_enabled = Some(enabled);
        self
    }
}

impl FromScalar<1> for UpdateKioskPolicy {
    fn from_scalar([flags]: [u32; 1]) -> Self {
        let present_flags = flags >> 16;
        let value_flags = flags & u16::MAX as u32;
        let read_flag = |flag| (present_flags & flag != 0).then_some(value_flags & flag != 0);
        Self {
            auto_lock_enabled: read_flag(Self::AUTO_LOCK),
            control_center_enabled: read_flag(Self::CONTROL_CENTER),
            home_button_enabled: read_flag(Self::HOME_BUTTON),
            power_button_enabled: read_flag(Self::POWER_BUTTON),
        }
    }
}

impl AsScalar<1> for UpdateKioskPolicy {
    fn as_scalar(&self) -> [u32; 1] {
        let mut present_flags = 0;
        let mut value_flags = 0;
        for (flag, value) in [
            (Self::AUTO_LOCK, self.auto_lock_enabled),
            (Self::CONTROL_CENTER, self.control_center_enabled),
            (Self::HOME_BUTTON, self.home_button_enabled),
            (Self::POWER_BUTTON, self.power_button_enabled),
        ] {
            if let Some(enabled) = value {
                present_flags |= flag;
                if enabled {
                    value_flags |= flag;
                }
            }
        }
        [(present_flags << 16) | value_flags]
    }
}

impl FromScalar<1> for AnimateNextFrame {
    fn from_scalar([animation_kind]: [u32; 1]) -> Self {
        Self { animation_kind: NextFrameAnimationKind::from_u32(animation_kind).unwrap_or_default() }
    }
}

impl AsScalar<1> for AnimateNextFrame {
    fn as_scalar(&self) -> [u32; 1] { [self.animation_kind as u32] }
}
