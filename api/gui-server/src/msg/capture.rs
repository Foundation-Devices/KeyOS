// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Messages for device capture (screenshot), touch injection, and key injection.
//! These work on both hardware and simulator — used by the passport-drive debug bridge.

use num_traits::{FromPrimitive, ToPrimitive};
use server::{AsScalar, FromScalar, SimpleMemoryMessage};
use xous::MemoryRange;

use crate::touch::{Touch, TouchKind};
use crate::Key;

/// Captures the current composited screen contents as raw pixel data.
///
/// On hardware the byte order is BGRA8888; on the simulator it is RGBA8888.
/// Caller allocates a MemoryRange of SCREEN_WIDTH * SCREEN_HEIGHT * 4 bytes, lends it
/// mutable to gui-server, which fills it with the composited pixel data.
#[derive(Debug, server::Message)]
#[response(())]
pub struct CaptureScreen(pub MemoryRange);

impl From<SimpleMemoryMessage> for CaptureScreen {
    fn from(value: SimpleMemoryMessage) -> Self { Self(value.buf) }
}

impl From<CaptureScreen> for SimpleMemoryMessage {
    fn from(val: CaptureScreen) -> Self { SimpleMemoryMessage { buf: val.0, arg1: 0, arg2: 0 } }
}

/// Injects a touch event into the GUI event pipeline as if it came from hardware.
/// Works on both hardware and simulator.
#[derive(Debug, server::Message)]
pub struct InjectTouch(pub Touch);

/// Injects a key press or release event into the active app as if it came from the keyboard.
/// Works on both hardware and simulator — used by the passport-drive debug bridge.
#[derive(Debug, server::Message)]
pub struct InjectKey {
    pub is_pressed: bool,
    pub key: Key,
}

impl AsScalar<3> for InjectKey {
    fn as_scalar(&self) -> [u32; 3] {
        let [kind, val] = self.key.as_scalar();
        [u32::from(self.is_pressed), kind, val]
    }
}

impl FromScalar<3> for InjectKey {
    fn from_scalar([is_pressed, kind, val]: [u32; 3]) -> Self {
        Self { is_pressed: is_pressed != 0, key: Key::from_scalar([kind, val]) }
    }
}

// AsScalar / FromScalar impls for Touch — defined here (not in simulator.rs)
// so they are available on hardware builds too.
impl FromScalar<4> for Touch {
    fn from_scalar([kind, id, x, y]: [u32; 4]) -> Self {
        Touch {
            kind: TouchKind::from_u32(kind).unwrap_or(TouchKind::Press),
            id: id as usize,
            x: x as usize,
            y: y as usize,
        }
    }
}

impl AsScalar<4> for Touch {
    fn as_scalar(&self) -> [u32; 4] {
        [self.kind.to_u32().unwrap(), self.id as u32, self.x as u32, self.y as u32]
    }
}
