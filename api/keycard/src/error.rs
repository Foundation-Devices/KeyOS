// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{AsScalar, FromScalar};

#[derive(
    Debug, Clone, Copy, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, PartialEq,
)]
pub enum KeycardError {
    #[error("OS error: {:?}", xous::Error::from_usize(*.0))]
    Xous(usize),
    #[error("Security error: AccessDenied")]
    Security(usize),
    #[error("Shamir error: {0:?}")]
    Shamir(crypto::error::ShamirError),
    #[error("Crypto error: {0:?}")]
    Crypto(crypto::error::CryptoError),
    #[error("Nfc error: {0:?}")]
    Nfc(nfc::error::NfcError),
    #[error("Ndef error")]
    Ndef,
    #[error("No more shards left")]
    NoShardLeft,
    #[error("Not a magic backup shard")]
    NotMagicBackupShard,
    #[error("Invalid data")]
    InvalidData,
    #[error("Different hardware UID")]
    DifferentDeviceId,
    #[error("Seed missing")]
    SeedMissing,
    #[error("Different seed fingerprint")]
    DifferentSeedFingerprint,
    #[error("Not enough shards")]
    NotEnoughShards,
    #[error("HMAC mismatch")]
    HmacMismatch,
    #[error("Blank shard")]
    BlankShard,
    #[error("Blank tag")]
    BlankTag,
    #[error("Invalid shamir scheme")]
    InvalidScheme,
    #[error("Shard scheme doesn't match active scheme")]
    SchemeMismatch,
    #[error("Shard timestamp {found} doesn't match expected {expected}")]
    TimestampMismatch { expected: u32, found: u32 },
    #[error("Other error")]
    Other,
}

#[derive(Clone, Copy, Debug, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum KeycardIdentifyError {
    #[error("Invalid data")]
    InvalidData,
    #[error("Different device ID")]
    DifferentDeviceId,
    #[error("Different seed fingerprint")]
    DifferentSeedFingerprint,
    #[error("Unauthenticated shard")]
    HmacMismatch,
    #[error("Existing shard")]
    ExistingShard,
}

impl From<xous::Error> for KeycardError {
    fn from(value: xous::Error) -> Self { KeycardError::Xous(value.to_usize()) }
}

impl From<security::AccessDenied> for KeycardError {
    fn from(_: security::AccessDenied) -> Self { KeycardError::Security(0) }
}

impl From<security::GetDeviceIdError> for KeycardError {
    fn from(_: security::GetDeviceIdError) -> Self { KeycardError::Security(1) }
}

impl From<crypto::error::ShamirError> for KeycardError {
    fn from(value: crypto::error::ShamirError) -> Self { Self::Shamir(value) }
}

impl From<crypto::error::CryptoError> for KeycardError {
    fn from(value: crypto::error::CryptoError) -> Self { Self::Crypto(value) }
}

impl From<nfc::error::NfcError> for KeycardError {
    fn from(value: nfc::error::NfcError) -> Self { Self::Nfc(value) }
}

impl AsScalar<3> for KeycardError {
    fn as_scalar(&self) -> [u32; 3] {
        match self {
            KeycardError::Xous(e) => [1, *e as u32, 0],
            KeycardError::Security(e) => [2, *e as u32, 0],
            KeycardError::Shamir(e) => [3, AsScalar::<1>::as_scalar(e)[0], 0],
            KeycardError::Crypto(e) => [4, AsScalar::<1>::as_scalar(e)[0], 0],
            KeycardError::Nfc(e) => [5, AsScalar::<1>::as_scalar(e)[0], 0],
            KeycardError::Ndef => [6, 0, 0],
            KeycardError::NoShardLeft => [7, 0, 0],
            KeycardError::NotMagicBackupShard => [8, 0, 0],
            KeycardError::InvalidData => [9, 0, 0],
            KeycardError::DifferentDeviceId => [10, 0, 0],
            KeycardError::DifferentSeedFingerprint => [11, 0, 0],
            KeycardError::SeedMissing => [12, 0, 0],
            KeycardError::NotEnoughShards => [13, 0, 0],
            KeycardError::HmacMismatch => [14, 0, 0],
            KeycardError::BlankShard => [15, 0, 0],
            KeycardError::BlankTag => [16, 0, 0],
            KeycardError::InvalidScheme => [17, 0, 0],
            KeycardError::SchemeMismatch => [18, 0, 0],
            KeycardError::TimestampMismatch { expected, found } => [19, *expected, *found],
            KeycardError::Other => [0, 0, 0],
        }
    }
}

impl FromScalar<3> for KeycardError {
    fn from_scalar(value: [u32; 3]) -> Self {
        match value[0] {
            1 => KeycardError::Xous(value[1] as usize),
            2 => KeycardError::Security(value[1] as usize),
            3 => KeycardError::Shamir(crypto::error::ShamirError::from_scalar([value[1]])),
            4 => KeycardError::Crypto(crypto::error::CryptoError::from_scalar([value[1]])),
            5 => KeycardError::Nfc(nfc::error::NfcError::from_scalar([value[1]])),
            6 => KeycardError::Ndef,
            7 => KeycardError::NoShardLeft,
            8 => KeycardError::NotMagicBackupShard,
            9 => KeycardError::InvalidData,
            10 => KeycardError::DifferentDeviceId,
            11 => KeycardError::DifferentSeedFingerprint,
            12 => KeycardError::SeedMissing,
            13 => KeycardError::NotEnoughShards,
            14 => KeycardError::HmacMismatch,
            15 => KeycardError::BlankShard,
            16 => KeycardError::BlankTag,
            17 => KeycardError::InvalidScheme,
            18 => KeycardError::SchemeMismatch,
            19 => KeycardError::TimestampMismatch { expected: value[1], found: value[2] },
            _ => KeycardError::Other,
        }
    }
}

impl From<usize> for KeycardError {
    fn from(value: usize) -> Self { Self::from_scalar([value as u32, 0, 0]) }
}

impl From<KeycardError> for usize {
    fn from(value: KeycardError) -> Self { server::AsScalar::<3>::as_scalar(&value)[0] as usize }
}
