use zerocopy::{IntoBytes, TryFromBytes};

use crate::error::MassStorageError;

pub const INVALID_COMMAND_OPERATION_CODE: (u8, u8) = (0x20, 0x00);
pub const INVALID_FIELD_IN_CBD: (u8, u8) = (0x24, 0x00);
pub const LOGICAL_BLOCK_ADDRESS_OUT_OF_RANGE: (u8, u8) = (0x21, 0x00);
pub const LOGICAL_UNIT_FAILURE: (u8, u8) = (0x3e, 0x01);
pub const WRITE_PROTECTED: (u8, u8) = (0x27, 0x00);

#[derive(
    Debug, Clone, zerocopy::KnownLayout, zerocopy::Immutable, zerocopy::IntoBytes, zerocopy::TryFromBytes,
)]
#[repr(C, packed)]
pub struct Cbw {
    signature: u32,
    tag: u32,
    transfer_len: u32,
    flags: u8,
    lun: u8,
    cb_len: u8,
    cb: [u8; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbwDirection {
    In,
    Out,
}

#[derive(
    Debug, Clone, zerocopy::KnownLayout, zerocopy::Immutable, zerocopy::IntoBytes, zerocopy::TryFromBytes,
)]
#[repr(C, packed)]
pub struct Csw {
    signature: u32,
    tag: u32,
    data_residue: u32,
    status: CswStatus,
}

#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    zerocopy::IntoBytes,
    zerocopy::TryFromBytes,
)]
#[repr(u8)]
pub enum CswStatus {
    Passed = 0,
    Failed = 1,
    #[allow(dead_code)]
    PhaseError = 2,
}

#[derive(
    Debug, Clone, zerocopy::KnownLayout, zerocopy::Immutable, zerocopy::IntoBytes, zerocopy::TryFromBytes,
)]
#[repr(u8)]
#[allow(dead_code)] // We 'construct' those variants with zerocopy TryFromBytes
pub enum Command {
    TestUnitReady(Typical6) = 0x00,
    RequestSense(Typical6) = 0x03,
    Inquiry(Inquiry6) = 0x12,
    Read6(Typical6) = 0x08,
    Read10(Typical10) = 0x28,
    Write6(Typical6) = 0x0a,
    Write10(Typical10) = 0x2a,
    SynchronizeCache10(Typical10) = 0x35,
    ReadCapacity10(Typical10) = 0x25,
    ReportLuns(Typical12) = 0xA0,
    ModeSense6(ModeSense6) = 0x1A,
    PreventAllowMediumRemoval([u8; 15]) = 0x1e,
    ReadFormatCapacity([u8; 15]) = 0x23,
    StartStopUnit([u8; 15]) = 0x1b,
}

const _: () = assert!(size_of::<Command>() == 16);

#[derive(Debug, Clone, Default, zerocopy::Immutable, zerocopy::IntoBytes, zerocopy::TryFromBytes)]
#[repr(C, packed)]
pub struct Typical6 {
    pub flags: u8,
    pub lba: zerocopy::big_endian::U16,
    pub length: u8,
    pub control: u8,
    pub _padding: [u8; 10],
}

#[derive(Debug, Clone, Default, zerocopy::Immutable, zerocopy::IntoBytes, zerocopy::TryFromBytes)]
#[repr(C, packed)]
pub struct Typical10 {
    pub flags: u8,
    pub lba: zerocopy::big_endian::U32,
    pub _reserved: u8,
    pub length: zerocopy::big_endian::U16,
    pub control: u8,
    pub _padding: [u8; 6],
}

#[derive(Debug, Clone, Default, zerocopy::Immutable, zerocopy::IntoBytes, zerocopy::TryFromBytes)]
#[repr(C, packed)]
pub struct Typical12 {
    pub flags: u8,
    pub lba: zerocopy::big_endian::U32,
    pub length: zerocopy::big_endian::U32,
    pub _reserved: u8,
    pub control: u8,
    pub _padding: [u8; 4],
}

#[derive(Debug, Clone, Default, zerocopy::Immutable, zerocopy::IntoBytes, zerocopy::TryFromBytes)]
#[repr(C, packed)]
pub struct ModeSense6 {
    pub flags: u8,
    pub page: u8,
    pub _reserved: u8,
    pub length: u8,
    pub control: u8,
    pub _padding: [u8; 10],
}

#[derive(Debug, Clone, Default, zerocopy::Immutable, zerocopy::IntoBytes, zerocopy::TryFromBytes)]
#[repr(C, packed)]
pub struct Inquiry6 {
    pub flags: u8,
    pub page: u8,
    pub _reserved: u8,
    pub length: u8,
    pub control: u8,
    pub _padding: [u8; 10],
}

#[derive(
    Debug,
    Clone,
    Default,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    zerocopy::IntoBytes,
    zerocopy::TryFromBytes,
)]
#[repr(C, packed)]
pub struct SenseResponse {
    pub response_code: u8,
    pub _obsolete: u8,
    pub flags_and_sense_key: SenseKey,
    pub information: zerocopy::big_endian::U32,
    pub additional_sense_length: u8,
    pub command_specific_information: zerocopy::big_endian::U32,
    pub additional_sense_code: u8,
    pub additional_sense_code_qualifier: u8,
    pub fru_code: u8,
    pub sense_key_specific: [u8; 3],
}

#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    zerocopy::IntoBytes,
    zerocopy::TryFromBytes,
)]
#[cfg_attr(feature = "rkyv", derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize))]
#[repr(u8)]
#[allow(missing_docs)]
pub enum SenseKey {
    #[default]
    NoSense = 0,
    RecoveredError = 1,
    NotReady = 2,
    MediumError = 3,
    HardwareError = 4,
    IllegalRequest = 5,
    UnitAttention = 6,
    DataProtect = 7,
    BlankCheck = 8,
    VendorSpecific = 9,
    CopyAborted = 10,
    AbortedCommand = 11,
    _Obsolete = 12,
    VolumeOverflow = 13,
    Miscompare = 14,
    _Reserved = 15,
}

#[derive(
    Debug,
    Clone,
    Default,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    zerocopy::IntoBytes,
    zerocopy::TryFromBytes,
)]
#[repr(C, packed)]
pub struct CapacityResponse {
    pub last_block: zerocopy::big_endian::U32,
    pub block_size: zerocopy::big_endian::U32,
}

#[derive(
    Debug,
    Clone,
    Default,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    zerocopy::IntoBytes,
    zerocopy::TryFromBytes,
)]
#[repr(C, packed)]
pub struct FormatCapacityResponse {
    pub reserved: [u8; 3],
    pub list_length: u8,
    pub number_of_blocks: zerocopy::big_endian::U32,
    pub descriptor_type: u8,
    pub block_length: [u8; 3],
}

impl Cbw {
    pub fn new(transfer_len: u32, lun: u8, command: Command) -> Self {
        Self {
            signature: 0x43425355, // "USBC"
            tag: 0,                // To be set just before send
            transfer_len,
            flags: 0, // To be set via set_direction
            lun,
            cb_len: command.len() as u8,
            cb: command.as_bytes().try_into().unwrap(),
        }
    }

    pub fn into_bytes(self) -> [u8; 31] { self.as_bytes().try_into().unwrap() }

    pub fn set_tag(&mut self, tag: u32) { self.tag = tag; }

    pub fn set_direction(&mut self, direction: CbwDirection) {
        self.flags = if direction == CbwDirection::In { 0x80 } else { 0 }
    }

    pub fn check(&self) -> Result<(), MassStorageError> {
        if self.signature != 0x43425355 || self.cb_len > 16 {
            return Err(MassStorageError::InvalidCbw);
        }
        Ok(())
    }

    pub fn command(&self) -> Result<&Command, MassStorageError> {
        Command::try_ref_from_bytes(&self.cb).map_err(|_| MassStorageError::InvalidCbwCommand)
    }

    pub fn tag(&self) -> u32 { self.tag }

    pub fn lun(&self) -> u8 { self.lun }
}

impl Csw {
    pub fn new() -> Self {
        Self { signature: 0x53425355, tag: 0, data_residue: 0, status: CswStatus::Passed }
    }

    pub fn failed(mut self) -> Self {
        self.status = CswStatus::Failed;
        self
    }

    pub fn with_tag(mut self, tag: u32) -> Self {
        self.tag = tag;
        self
    }

    pub fn check(&self, tag: u32) -> Result<(), MassStorageError> {
        if self.signature != 0x53425355
        /* "USBS" */
        {
            Err(MassStorageError::InvalidCswSignature)
        } else if self.tag != tag {
            Err(MassStorageError::InvalidCswTag)
        } else {
            match self.status {
                CswStatus::Passed => Ok(()),
                CswStatus::Failed => Err(MassStorageError::CommandFailed),
                CswStatus::PhaseError => Err(MassStorageError::PhaseError),
            }
        }
    }
}

impl Command {
    fn len(&self) -> usize {
        match self {
            Command::Inquiry(_) => 6,
            Command::Read6(_) => 6,
            Command::Read10(_) => 10,
            Command::ReadCapacity10(_) => 10,
            Command::ReportLuns(_) => 12,
            Command::RequestSense(_) => 6,
            Command::TestUnitReady(_) => 6,
            Command::Write6(_) => 6,
            Command::Write10(_) => 10,
            Command::SynchronizeCache10(_) => 10,
            Command::ModeSense6(_) => 6,
            Command::PreventAllowMediumRemoval(_) => 6,
            Command::ReadFormatCapacity(_) => 10,
            Command::StartStopUnit(_) => 6,
        }
    }
}
