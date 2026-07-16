// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{AsScalar, FromScalar};

#[derive(Debug, server::Message)]
pub struct ShowCamera {
    pub y_pos: u16,
}
impl FromScalar<1> for ShowCamera {
    fn from_scalar([y_pos]: [u32; 1]) -> Self { Self { y_pos: y_pos as u16 } }
}
impl AsScalar<1> for ShowCamera {
    fn as_scalar(&self) -> [u32; 1] { [self.y_pos as u32] }
}

#[derive(Debug, server::Message)]
pub struct HideCamera;
