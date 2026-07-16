// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use xous::Error;
use {
    num_derive::{FromPrimitive, ToPrimitive},
    num_traits::{FromPrimitive, ToPrimitive},
};

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, FromPrimitive, ToPrimitive)]
pub enum I2cError {
    AlreadyClaimed = 1,
    PeripheralNotClaimed,
    AccessDenied,
    Nack,
    Timeout,
    UnsupportedDataSize,
    InternalError,
}

impl eh_1::i2c::Error for I2cError {
    fn kind(&self) -> eh_1::i2c::ErrorKind {
        match self {
            I2cError::InternalError => eh_1::i2c::ErrorKind::Bus,
            _ => eh_1::i2c::ErrorKind::Other,
        }
    }
}

impl server::AsScalar<1> for I2cError {
    fn as_scalar(&self) -> [u32; 1] { [self.to_u32().unwrap()] }
}

impl server::FromScalar<1> for I2cError {
    fn from_scalar(value: [u32; 1]) -> Self { Self::from_u32(value[0]).unwrap_or(Self::InternalError) }
}

impl From<xous::Error> for I2cError {
    fn from(_value: Error) -> Self { I2cError::InternalError }
}
