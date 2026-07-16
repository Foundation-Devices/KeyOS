// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{AsScalar, FromScalar};

#[derive(Debug, Clone, Copy, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum FidoError {
    #[error("OS error: {:?}", xous::Error::from_usize(*.0))]
    Xous(usize),
    #[error("Crypto error: {0:?}")]
    Crypto(crypto::error::CryptoError),
    #[error("Security Access denied")]
    AccessDenied,
    #[error("FS error: {0:?}")]
    Fs(fs::Error),
    #[error("ECDSA error")]
    Ecdsa,
    #[error("Invalid index")]
    InvalidIndex,
    #[error("Security Key not selected")]
    UnselectedKey,
    #[error("Key not registered in selected Security Key")]
    UnRegisteredKey,
    #[error("IO error")]
    Io,
    #[error("Label is empty")]
    EmptyLabel,
    #[error("Label already in use")]
    DuplicateLabel,
    #[error("Other Fido error")]
    Other,
}

impl From<xous::Error> for FidoError {
    fn from(value: xous::Error) -> Self { FidoError::Xous(value.to_usize()) }
}

impl From<crypto::error::CryptoError> for FidoError {
    fn from(value: crypto::error::CryptoError) -> Self { Self::Crypto(value) }
}

impl From<security::AccessDenied> for FidoError {
    fn from(_: security::AccessDenied) -> Self { Self::AccessDenied }
}

impl From<fs::Error> for FidoError {
    fn from(value: fs::Error) -> Self { Self::Fs(value) }
}

impl From<std::io::Error> for FidoError {
    fn from(_value: std::io::Error) -> Self { Self::Io }
}

impl AsScalar<3> for FidoError {
    fn as_scalar(&self) -> [u32; 3] {
        match self {
            FidoError::Xous(e) => [1, *e as u32, 0],
            FidoError::Crypto(e) => [3, AsScalar::<1>::as_scalar(e)[0], 0],
            FidoError::AccessDenied => [4, 0, 0],
            FidoError::Fs(e) => [5, *e as u32, 0],
            FidoError::Ecdsa => [6, 0, 0],
            FidoError::InvalidIndex => [7, 0, 0],
            FidoError::UnselectedKey => [8, 0, 0],
            FidoError::UnRegisteredKey => [9, 0, 0],
            FidoError::Io => [10, 0, 0],
            FidoError::EmptyLabel => [12, 0, 0],
            FidoError::DuplicateLabel => [13, 0, 0],
            FidoError::Other => [11, 0, 0],
        }
    }
}

impl FromScalar<3> for FidoError {
    fn from_scalar(value: [u32; 3]) -> Self {
        match value[0] {
            1 => FidoError::Xous(value[1] as usize),
            3 => FidoError::Crypto(crypto::error::CryptoError::from_scalar([value[1]])),
            4 => FidoError::AccessDenied,
            5 => FidoError::Fs((value[1] as usize).into()),
            6 => FidoError::Ecdsa,
            7 => FidoError::InvalidIndex,
            8 => FidoError::UnselectedKey,
            9 => FidoError::UnRegisteredKey,
            10 => FidoError::Io,
            12 => FidoError::EmptyLabel,
            13 => FidoError::DuplicateLabel,
            _ => FidoError::Other,
        }
    }
}

impl From<usize> for FidoError {
    fn from(value: usize) -> Self { Self::from_scalar([value as u32, 0, 0]) }
}

impl From<FidoError> for usize {
    fn from(value: FidoError) -> Self { server::AsScalar::<3>::as_scalar(&value)[0] as usize }
}
