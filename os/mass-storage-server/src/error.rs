// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[derive(Debug, thiserror::Error)]
pub enum MassStorageError {
    #[error("OS error: {:?}", xous::Error::from_usize(*.0))]
    XousError(usize),
    #[error("Mass storage error: {0}")]
    MassStorageError(mass_storage::MassStorageError),
    #[error("The device is write-protected")]
    WriteProtected,
    #[error("Mass storage device is not connected")]
    NotConnected,
    #[error("Buffer was not a multiple of block size")]
    UnalignedBufferSize,
    #[error("Other error")]
    Other,
}

impl From<xous::Error> for MassStorageError {
    fn from(value: xous::Error) -> Self { Self::XousError(value.to_usize()) }
}

impl From<mass_storage::MassStorageError> for MassStorageError {
    fn from(value: mass_storage::MassStorageError) -> Self { Self::MassStorageError(value) }
}

impl server::AsScalar<2> for MassStorageError {
    fn as_scalar(&self) -> [u32; 2] {
        match self {
            MassStorageError::XousError(e) => [1, *e as u32],
            MassStorageError::MassStorageError(e) => [2, u32::from(u16::from(*e))],
            MassStorageError::WriteProtected => [3, 0],
            MassStorageError::NotConnected => [4, 0],
            MassStorageError::UnalignedBufferSize => [5, 0],
            MassStorageError::Other => [6, 0],
        }
    }
}

impl server::FromScalar<2> for MassStorageError {
    fn from_scalar(value: [u32; 2]) -> Self {
        match value[0] {
            1 => MassStorageError::XousError(value[1] as usize),
            2 => MassStorageError::MassStorageError((value[1] as u16).into()),
            3 => MassStorageError::WriteProtected,
            4 => MassStorageError::NotConnected,
            5 => MassStorageError::UnalignedBufferSize,
            _ => MassStorageError::Other,
        }
    }
}

impl From<usize> for MassStorageError {
    fn from(value: usize) -> Self {
        server::FromScalar::from_scalar([value as u32 >> 24, value as u32 & 0xFFFFFF])
    }
}

impl From<MassStorageError> for usize {
    fn from(value: MassStorageError) -> Self {
        let [h, l] = server::AsScalar::as_scalar(&value);
        ((h << 24) | l) as usize
    }
}
