// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use num_traits::FromPrimitive;
use server::{AsScalar, FromScalar};
use xous::{MemoryRange, PID};

mod camera;
mod capture;
mod control_center;
mod framebuffer;
mod keyboard;
mod navigation;
mod register;
mod scalar;
#[cfg(not(feature = "recovery-os"))]
mod settings;
#[cfg(not(keyos))]
mod simulator;

// GuiServer => GuiServer messages not exposed in the API crate:

#[derive(Debug, server::Message)]
pub(crate) struct DisconnectHandlerMessage(xous::CID);

#[derive(Debug, Clone, Copy, server::Message)]
pub(crate) struct OnVsyncMessage;

#[derive(Debug, Clone, Copy, server::Message)]
pub(crate) struct OnFreeMemoryBelowThreshold;

#[derive(Debug, Clone, Copy, server::Message)]
pub(crate) struct CloseAppTimeout;

#[derive(Debug, Clone, Copy, server::Message)]
pub(crate) struct ForceShutdownTimeout;

#[derive(Debug, server::Message)]
pub(crate) struct BlurDone {
    pub pid: PID,
    pub buffer: MemoryRange,
}

impl FromScalar<4> for BlurDone {
    fn from_scalar(value: [u32; 4]) -> Self {
        Self {
            pid: PID::from_scalar([value[0]]),
            buffer: MemoryRange::from_scalar([value[1], value[2], value[3]]),
        }
    }
}

impl AsScalar<4> for BlurDone {
    fn as_scalar(&self) -> [u32; 4] {
        let [r0] = self.pid.as_scalar();
        let [r1, r2, r3] = self.buffer.as_scalar();
        [r0, r1, r2, r3]
    }
}

#[derive(Debug, server::Message)]
pub struct PowerButtonTimerCallback;

#[derive(Debug, server::Message)]
pub struct AutoLockTimerCallback(pub AutoLockStep);

#[derive(Debug, PartialEq, num_derive::FromPrimitive, num_derive::ToPrimitive, Copy, Clone)]
pub enum AutoLockStep {
    Dim,
    LcdOff,
    PowerOff,
}

impl server::AsScalar<1> for AutoLockStep {
    fn as_scalar(&self) -> [u32; 1] { [*self as u32] }
}

impl server::FromScalar<1> for AutoLockStep {
    fn from_scalar([value]: [u32; 1]) -> Self { Self::from_u32(value).unwrap_or(Self::PowerOff) }
}
