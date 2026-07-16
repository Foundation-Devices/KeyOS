// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{xous, AsScalar, FromScalar};

#[derive(
    Debug, Clone, Copy, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, PartialEq,
)]
pub enum CryptoError {
    #[error("OS error: {:?}", xous::Error::from_usize(*.0))]
    XousError(usize),

    #[error("Unknown internal error")]
    UnknownError,

    // These are errors that can't really happen through the API.
    #[error("Invalid parameter")]
    InvalidParameter,

    #[cfg(keyos)]
    #[error("There was a problem with the DMA engine")]
    DmaError,

    #[error("The source or destination address points to somewhere it shouldn't")]
    InvalidAddress,

    #[error("The length did not fit into the buffer or was 0")]
    InvalidDataLength,

    #[error("The lent buffer didn't contain contigous pages (use POPULATE)")]
    BufferNotContiguous,

    #[error("The process allocated more AES contexts than available")]
    TooManyAesContexts,

    #[error("No more SECURAM key slots left")]
    TooManySecuramKeys,

    #[error("The supplied key was a supported size")]
    InvalidKeyLength,

    #[error("The buffer length was not divisible by block length")]
    UnalignedDataLength,

    #[error("The AES context is in invalid operation mode for the method")]
    InvalidMode,

    #[error("The AES context is in a state that does not allow this operation")]
    InvalidState,
}

impl From<xous::Error> for CryptoError {
    fn from(value: xous::Error) -> Self { CryptoError::XousError(value.to_usize()) }
}

impl From<()> for CryptoError {
    fn from(_: ()) -> Self { CryptoError::UnknownError }
}

impl From<CryptoError> for usize {
    fn from(value: CryptoError) -> usize {
        match value {
            CryptoError::XousError(e) => e,
            CryptoError::UnknownError => 0x80000001,
            CryptoError::InvalidParameter => 0x80000002,
            #[cfg(keyos)]
            CryptoError::DmaError => 0x80000003,
            CryptoError::InvalidAddress => 0x80000004,
            CryptoError::InvalidDataLength => 0x80000005,
            CryptoError::BufferNotContiguous => 0x80000006,
            CryptoError::TooManyAesContexts => 0x80000007,
            CryptoError::TooManySecuramKeys => 0x80000008,
            CryptoError::InvalidKeyLength => 0x80000009,
            CryptoError::UnalignedDataLength => 0x8000000b,
            CryptoError::InvalidMode => 0x8000000c,
            CryptoError::InvalidState => 0x8000000d,
        }
    }
}

impl From<usize> for CryptoError {
    fn from(value: usize) -> Self {
        match value {
            0x80000001 => CryptoError::UnknownError,
            0x80000002 => CryptoError::InvalidParameter,
            #[cfg(keyos)]
            0x80000003 => CryptoError::DmaError,
            0x80000004 => CryptoError::InvalidAddress,
            0x80000005 => CryptoError::InvalidDataLength,
            0x80000006 => CryptoError::BufferNotContiguous,
            0x80000007 => CryptoError::TooManyAesContexts,
            0x80000008 => CryptoError::TooManySecuramKeys,
            0x80000009 => CryptoError::InvalidKeyLength,
            0x8000000b => CryptoError::UnalignedDataLength,
            0x8000000c => CryptoError::InvalidMode,
            0x8000000d => CryptoError::InvalidState,
            _ => CryptoError::XousError(value),
        }
    }
}

impl AsScalar<1> for CryptoError {
    fn as_scalar(&self) -> [u32; 1] { [usize::from(*self) as u32] }
}

impl FromScalar<1> for CryptoError {
    fn from_scalar([value]: [u32; 1]) -> Self { Self::from(value as usize) }
}

#[derive(Debug, Clone, Copy, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, PartialEq)]
pub enum ShamirError {
    SecretTooLong,
    TooManyShares,
    InterpolationFailure,
    ChecksumFailure,
    SecretTooShort,
    SecretNotEvenLen,
    InvalidThreshold,
    SharesUnequalLength,
}

impl AsScalar<1> for ShamirError {
    fn as_scalar(&self) -> [u32; 1] {
        match self {
            Self::SecretTooLong => [1],
            Self::TooManyShares => [2],
            Self::InterpolationFailure => [3],
            Self::ChecksumFailure => [4],
            Self::SecretTooShort => [5],
            Self::SecretNotEvenLen => [6],
            Self::InvalidThreshold => [7],
            Self::SharesUnequalLength => [8],
        }
    }
}

impl FromScalar<1> for ShamirError {
    fn from_scalar([value]: [u32; 1]) -> Self {
        match value {
            1 => Self::SecretTooLong,
            2 => Self::TooManyShares,
            3 => Self::InterpolationFailure,
            4 => Self::ChecksumFailure,
            5 => Self::SecretTooShort,
            6 => Self::SecretNotEvenLen,
            7 => Self::InvalidThreshold,
            8 => Self::SharesUnequalLength,
            _ => unreachable!(),
        }
    }
}
