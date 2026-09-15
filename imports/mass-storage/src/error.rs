use zerocopy::TryFromBytes;

use crate::commands::{
    SenseKey, INVALID_FIELD_IN_CBD, LOGICAL_BLOCK_ADDRESS_OUT_OF_RANGE, LOGICAL_UNIT_FAILURE,
};

/// General errors coming from the crate
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[cfg_attr(feature = "rkyv", derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize))]
#[allow(missing_docs)]
pub enum MassStorageError {
    #[error("USB error: {0}")]
    Usb(UsbError),
    #[error("Block device error: {0}")]
    BlockDevice(BlockDeviceError),
    #[error("Wrong length field in a transfer")]
    InvalidLength,
    #[error("Wrong CSW signature")]
    InvalidCswSignature,
    #[error("The CSW tag does not match the CBW")]
    InvalidCswTag,
    #[error("The CSW reported the command as failed")]
    CommandFailed,
    #[error("The SCSI command failed: {0:?}")]
    SenseError(SenseKey),
    #[error("The device reported a phase error and needs a reset")]
    PhaseError,
    #[error("Not a direct access device (e.g. a CDROM drive)")]
    NotDirectAccess,
    #[error("Wrong parameter")]
    InvalidArgument,
    #[error("Unknown error")]
    OtherError,
    #[error("The CBW was not valid or not meaningful")]
    InvalidCbw,
    #[error("The command in the CBW could not be parsed")]
    InvalidCbwCommand,
}

/// Usb error returned by the actual USB implementation
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[cfg_attr(feature = "rkyv", derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize))]
#[allow(missing_docs)]
pub enum UsbError {
    #[error("The endpoint stalled while transferring")]
    Stalled,
    #[error("The device was disconnected")]
    Disconnected,
    #[error("The transfer failed without the device refusing it")]
    TransferError,
    #[error("The host abandoned the transfer")]
    Cancelled,
    #[error("Unknown issue")]
    Other,
}

/// Block Device error returned by the actual Block Device implementation
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[cfg_attr(feature = "rkyv", derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize))]
#[allow(missing_docs)]
pub enum BlockDeviceError {
    #[error("Too many blocks in a single operation")]
    TooManyBlocks,
    #[error("Block index out of range")]
    OutOfRange,
    #[error("Unknown issue")]
    Other,
}

pub type Result<T> = core::result::Result<T, MassStorageError>;

impl From<UsbError> for MassStorageError {
    fn from(value: UsbError) -> Self { Self::Usb(value) }
}

impl From<BlockDeviceError> for MassStorageError {
    fn from(value: BlockDeviceError) -> Self { Self::BlockDevice(value) }
}

// Packed for transports that can only carry a scalar: the variant in the high
// byte, its payload in the low one. Unrecognized encodings decode to the
// matching `Other` variant.
impl From<MassStorageError> for u16 {
    fn from(value: MassStorageError) -> Self {
        let (variant, payload): (u8, u8) = match value {
            MassStorageError::Usb(e) => (1, e.into()),
            MassStorageError::BlockDevice(e) => (2, e.into()),
            MassStorageError::InvalidLength => (3, 0),
            MassStorageError::InvalidCswSignature => (4, 0),
            MassStorageError::InvalidCswTag => (5, 0),
            MassStorageError::CommandFailed => (6, 0),
            MassStorageError::SenseError(key) => (7, key as u8),
            MassStorageError::PhaseError => (8, 0),
            MassStorageError::NotDirectAccess => (9, 0),
            MassStorageError::InvalidArgument => (10, 0),
            MassStorageError::OtherError => (11, 0),
            MassStorageError::InvalidCbw => (12, 0),
            MassStorageError::InvalidCbwCommand => (13, 0),
        };
        (u16::from(variant) << 8) | u16::from(payload)
    }
}

impl From<u16> for MassStorageError {
    fn from(value: u16) -> Self {
        let payload = value as u8;
        match (value >> 8) as u8 {
            1 => Self::Usb(payload.into()),
            2 => Self::BlockDevice(payload.into()),
            3 => Self::InvalidLength,
            4 => Self::InvalidCswSignature,
            5 => Self::InvalidCswTag,
            6 => Self::CommandFailed,
            7 => SenseKey::try_read_from_bytes(&[payload]).map_or(Self::OtherError, Self::SenseError),
            8 => Self::PhaseError,
            9 => Self::NotDirectAccess,
            10 => Self::InvalidArgument,
            12 => Self::InvalidCbw,
            13 => Self::InvalidCbwCommand,
            _ => Self::OtherError,
        }
    }
}

impl From<UsbError> for u8 {
    fn from(value: UsbError) -> Self {
        match value {
            UsbError::Stalled => 1,
            UsbError::Disconnected => 2,
            UsbError::Other => 3,
            UsbError::TransferError => 4,
            UsbError::Cancelled => 5,
        }
    }
}

impl From<u8> for UsbError {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Stalled,
            2 => Self::Disconnected,
            4 => Self::TransferError,
            5 => Self::Cancelled,
            _ => Self::Other,
        }
    }
}

impl From<BlockDeviceError> for u8 {
    fn from(value: BlockDeviceError) -> Self {
        match value {
            BlockDeviceError::TooManyBlocks => 1,
            BlockDeviceError::OutOfRange => 2,
            BlockDeviceError::Other => 3,
        }
    }
}

impl From<u8> for BlockDeviceError {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::TooManyBlocks,
            2 => Self::OutOfRange,
            _ => Self::Other,
        }
    }
}

impl BlockDeviceError {
    pub(crate) fn sense_key(&self) -> SenseKey {
        match self {
            BlockDeviceError::TooManyBlocks => SenseKey::IllegalRequest,
            BlockDeviceError::OutOfRange => SenseKey::IllegalRequest,
            BlockDeviceError::Other => SenseKey::HardwareError,
        }
    }

    pub(crate) fn sense_code(&self) -> (u8, u8) {
        match self {
            BlockDeviceError::TooManyBlocks => INVALID_FIELD_IN_CBD,
            BlockDeviceError::OutOfRange => LOGICAL_BLOCK_ADDRESS_OUT_OF_RANGE,
            BlockDeviceError::Other => LOGICAL_UNIT_FAILURE,
        }
    }
}
