// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{AsScalar, FromScalar};

use crate::{RgbAnimation, RgbColor};

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(RgbColor)]
pub struct SubscribeColorUpdates;

/// Set a single LED to a color by index
#[derive(Debug, server::Message)]
pub struct SetTo(pub u32, pub RgbColor);

impl FromScalar<2> for SetTo {
    fn from_scalar(value: [u32; 2]) -> Self { SetTo(value[0], value[1].into()) }
}

impl AsScalar<2> for SetTo {
    fn as_scalar(&self) -> [u32; 2] { [self.0, self.1.into()] }
}

#[derive(Debug, server::Message)]
pub struct SetAllTo(pub RgbColor);

impl FromScalar<1> for RgbColor {
    fn from_scalar(value: [u32; 1]) -> Self { value[0].into() }
}

impl AsScalar<1> for RgbColor {
    fn as_scalar(&self) -> [u32; 1] { [(*self).into()] }
}

#[derive(Debug, server::Message)]
pub struct AnimateAllTo(pub RgbAnimation);

impl FromScalar<4> for RgbAnimation {
    fn from_scalar(value: [u32; 4]) -> Self {
        RgbAnimation {
            from: value[0].into(),
            to: value[1].into(),
            duration_ms: value[2] as usize,
            reset: value[3] != 0,
        }
    }
}

impl AsScalar<4> for RgbAnimation {
    fn as_scalar(&self) -> [u32; 4] {
        [self.from.into(), self.to.into(), self.duration_ms as u32, self.reset as u32]
    }
}
