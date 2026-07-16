// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
use {
    crate::{I2cError, Peripheral},
    num_traits::FromPrimitive,
    server::{AsScalar, FromScalar},
};

#[derive(Debug, server::Message)]
#[response(Result<(), I2cError>)]
pub struct ClaimPeripheral(pub Peripheral);

impl AsScalar<1> for Peripheral {
    fn as_scalar(&self) -> [u32; 1] { [*self as u32] }
}

impl FromScalar<1> for Peripheral {
    fn from_scalar([a]: [u32; 1]) -> Self { Peripheral::from_u32(a).expect("decode") }
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Vec<u8>, I2cError>)]
pub struct SingleTransfer {
    pub peripheral: Peripheral,
    pub write_data: Vec<u8>,
    pub read_len: usize,
}
