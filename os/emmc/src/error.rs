// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{AsScalar, FromScalar};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub enum EmmcError {
    #[error("OS error: {:?}", xous::Error::from_usize(*.0))]
    XousError(usize),
    #[error("There was a logic error in the server itself")]
    InternalError,
    #[error("SDMMC bus error")]
    SdmmcError,
    #[error("More blocks were requested than could be buffered")]
    BufferTooLarge,
    #[error("Buffer size was not a multiple of block count")]
    UnalignedBufferSize,
    #[error("Read or write would go beyond the capacity of the device")]
    OutOfRange,
    #[error("No blocks were requested")]
    EmptyRequest,
}

impl From<xous::Error> for EmmcError {
    fn from(value: xous::Error) -> Self { EmmcError::XousError(value.to_usize()) }
}

impl From<usize> for EmmcError {
    fn from(value: usize) -> Self {
        match value {
            0x80000000 => Self::InternalError,
            0x80000001 => Self::SdmmcError,
            0x80000002 => Self::BufferTooLarge,
            0x80000003 => Self::UnalignedBufferSize,
            0x80000004 => Self::OutOfRange,
            0x80000005 => Self::EmptyRequest,
            other => Self::XousError(other),
        }
    }
}

impl From<EmmcError> for usize {
    fn from(value: EmmcError) -> Self {
        match value {
            EmmcError::InternalError => 0x80000000,
            EmmcError::SdmmcError => 0x80000001,
            EmmcError::BufferTooLarge => 0x80000002,
            EmmcError::UnalignedBufferSize => 0x80000003,
            EmmcError::OutOfRange => 0x80000004,
            EmmcError::EmptyRequest => 0x80000005,
            EmmcError::XousError(other) => other,
        }
    }
}

impl From<crypto::error::CryptoError> for EmmcError {
    fn from(_value: crypto::error::CryptoError) -> Self { Self::InternalError }
}

impl AsScalar<1> for EmmcError {
    fn as_scalar(&self) -> [u32; 1] { [usize::from(*self) as u32] }
}

impl FromScalar<1> for EmmcError {
    fn from_scalar([v]: [u32; 1]) -> Self { Self::from(v as usize) }
}
