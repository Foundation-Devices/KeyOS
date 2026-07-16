// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{AsScalar, FromScalar, SimpleMemoryMessage};
use xous::MemoryRange;

#[derive(Debug, server::Message)]
#[response(())]
pub struct GetDeviceFrame(pub MemoryRange);

impl From<SimpleMemoryMessage> for GetDeviceFrame {
    fn from(value: SimpleMemoryMessage) -> Self { Self(value.buf) }
}

impl From<GetDeviceFrame> for SimpleMemoryMessage {
    fn from(val: GetDeviceFrame) -> Self { SimpleMemoryMessage { buf: val.0, arg1: 0, arg2: 0 } }
}

#[derive(Debug, server::Message)]
pub struct SetScaleFactor(pub usize);

#[derive(Debug, server::Message)]
pub struct SimulatePowerButton(pub bool);

#[derive(Debug, server::Message)]
pub struct SimulateKey {
    pub key: crate::Key,
    pub is_pressed: bool,
}

impl FromScalar<4> for SimulateKey {
    fn from_scalar([k1, k2, is_pressed, _]: [u32; 4]) -> Self {
        Self { key: crate::Key::from_scalar([k1, k2]), is_pressed: is_pressed != 0 }
    }
}

impl AsScalar<4> for SimulateKey {
    fn as_scalar(&self) -> [u32; 4] {
        let [k1, k2] = self.key.as_scalar();
        [k1, k2, self.is_pressed as u32, 0]
    }
}

/// Scroll delta from the host mouse/trackpad.
/// Position is in physical pixels; deltas are f32 scroll amounts (pixels).
#[derive(Debug, server::Message)]
pub struct SimulateScroll {
    pub x: u32,
    pub y: u32,
    pub delta_x: f32,
    pub delta_y: f32,
}

impl FromScalar<4> for SimulateScroll {
    fn from_scalar([x, y, dx, dy]: [u32; 4]) -> Self {
        Self { x, y, delta_x: f32::from_bits(dx), delta_y: f32::from_bits(dy) }
    }
}

impl AsScalar<4> for SimulateScroll {
    fn as_scalar(&self) -> [u32; 4] { [self.x, self.y, self.delta_x.to_bits(), self.delta_y.to_bits()] }
}
