// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    num_derive::{FromPrimitive, ToPrimitive},
    num_traits::{FromPrimitive, ToPrimitive},
};

#[derive(
    Debug,
    Copy,
    Clone,
    rkyv::Archive,
    thiserror::Error,
    rkyv::Serialize,
    rkyv::Deserialize,
    FromPrimitive,
    ToPrimitive,
    PartialEq,
)]
pub enum NfcError {
    #[error("Unknown error")]
    Unknown,
    #[error("Internal error")]
    Internal,
    #[error("Timeout error")]
    Timeout,
    #[error("NFC functionality disabled")]
    Disabled,
    #[error("Xous error")]
    Xous,
    #[error("Spi error")]
    Spi,
    #[error("Gpio error")]
    Gpio,
}

impl From<xous::Error> for NfcError {
    fn from(_value: xous::Error) -> Self { NfcError::Xous }
}

#[cfg(keyos)]
impl From<spi::SpiError> for NfcError {
    fn from(_value: spi::SpiError) -> Self { NfcError::Spi }
}

#[cfg(keyos)]
impl From<gpio::GpioApiError> for NfcError {
    fn from(_value: gpio::GpioApiError) -> Self { NfcError::Gpio }
}

impl From<()> for NfcError {
    fn from(_: ()) -> Self { NfcError::Unknown }
}

impl server::AsScalar<1> for NfcError {
    fn as_scalar(&self) -> [u32; 1] { [self.to_u32().unwrap()] }
}

impl server::FromScalar<1> for NfcError {
    fn from_scalar([value]: [u32; 1]) -> Self { Self::from_u32(value).unwrap_or(Self::Unknown) }
}
