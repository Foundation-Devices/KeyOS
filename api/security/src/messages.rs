// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{AsScalar, FromScalar};
use zeroize::ZeroizeOnDrop;

use crate::{
    AccessDenied, AppSeed, BluetoothChallengeSecret, DeviceId, FirmwareTimestamp, GetDeviceIdError,
    LockoutOptions, LoginFailed, MasterKeyState, OsVersionInfo, PinEntryMode, PinError, ScChallengeError,
    ScProof, SecurityWord, Seed,
};

#[derive(Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), PinError>)]
pub struct SetSeedAndPin {
    pub seed: Seed,
    pub pin: RawPin,
    pub pin_entry: PinEntryMode,
}

#[derive(Clone, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), PinError>)]
pub struct ChangePin {
    pub pin: RawPin,
    pub seed: Option<Seed>,
    pub pin_entry: PinEntryMode,
}

#[derive(Clone, ZeroizeOnDrop, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct RawPin(pub String);

#[derive(Clone, ZeroizeOnDrop, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), LoginFailed>)]
pub struct Login {
    pub pin: RawPin,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<u32, AccessDenied>)]
pub struct GetAttemptsRemaining;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<u32, AccessDenied>)]
pub struct GetFactoryResetCounter;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Option<Seed>, AccessDenied>)]
pub struct GetSeed;

#[derive(Clone, ZeroizeOnDrop, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), AccessDenied>)]
pub struct SetSeed(pub Seed);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<AppSeed, AccessDenied>)]
pub struct GetAppSeed;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<FirmwareTimestamp, AccessDenied>)]
pub struct GetFirmwareTimestamp;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), AccessDenied>)]
pub struct SetFirmwareTimestamp(pub FirmwareTimestamp);

#[derive(Debug, server::Message)]
#[response(())]
pub struct Logout;

#[derive(Debug, server::Message)]
#[response(bool)]
pub struct LoggedIn;

#[cfg(not(keyos))]
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(String)]
pub struct GetPin;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<(), AccessDenied>)]
pub struct Lockout {
    pub lockout_options: LockoutOptions,
    pub reboot: bool,
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<[u8; 64], AccessDenied>)]
pub struct SignWithSecurityCheckKey(pub [u8; 32]);

// Don't derive Debug: the prefix is the first four characters of the PIN.
#[derive(server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<[SecurityWord; 2], AccessDenied>)]
pub struct GetSecurityWords {
    pub pin_prefix: Vec<u8>,
}

#[cfg(not(keyos))]
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(())]
pub struct SetAttempts(pub u32);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<[u8; 32], AccessDenied>)]
pub struct GetSeedFingerprint;

#[derive(Clone, server::Message, ZeroizeOnDrop, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<[u8; 32], AccessDenied>)]
pub struct ComputeSeedFingerprint(pub Seed);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<[u8; 64], AccessDenied>)]
pub struct SignWithFidoKey(pub [u8; 32]);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<[u8; 64], AccessDenied>)]
pub struct GetFidoPubkey;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Option<OsVersionInfo>, AccessDenied>)]
pub struct GetOsVersionInfo;

/// A message to get the bootloader build date as a Unix timestamp.
/// Used by Recovery OS for bootloader rollback prevention during Core Recovery.
/// Added separately from `GetOsVersionInfo` for backward compatibility.
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<Option<u64>, AccessDenied>)]
pub struct GetBootloaderBuildDate;

/// A message sent from the server to the device. Includes the intermediate challenge, deadline and the server
/// signature, in the following binary format:
/// ```text
/// ------------------------------------
/// | challenge | deadline | signature |
/// | 32 bytes  | 8 bytes  | 64 bytes  |
/// ------------------------------------
/// ```
#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<ScProof, ScChallengeError>)]
pub struct ScChallenge(pub [u8; Self::SIZE]);

impl ScChallenge {
    pub const SIZE: usize = 104;
}

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<bool, AccessDenied>)]
pub struct IsPinSet;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<DeviceId, GetDeviceIdError>)]
pub struct GetDeviceId;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<[u8; 32], AccessDenied>)]
pub struct KeycardAuthenticityMac(pub [u8; 32]);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(BluetoothChallengeSecret)]
pub struct GetBluetoothChallengeSecret;

#[derive(Debug, server::Message)]
#[response(())]
pub struct SetBluetoothCheckSecretSent;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(())]
pub struct SetBluetoothDeviceId(pub [u8; 8]);

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(Result<[u8; 32], AccessDenied>)]
pub struct GetRandom;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[response(PinEntryMode)]
pub struct GetPinEntryMode;

#[derive(Debug, server::Message)]
#[response(MasterKeyState)]
pub struct GetMasterKeyState;

#[derive(Debug, server::Message, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[event(DiskEncryptionKeysReady)]
pub struct SubscribeDiskEncryptionKeysReady;

#[derive(Debug, Clone, Copy)]
pub struct DiskEncryptionKeysReady;

impl AsScalar<1> for DiskEncryptionKeysReady {
    fn as_scalar(&self) -> [u32; 1] { [0] }
}

impl FromScalar<1> for DiskEncryptionKeysReady {
    fn from_scalar(_: [u32; 1]) -> Self { Self }
}

impl FromScalar<1> for MasterKeyState {
    fn from_scalar([value]: [u32; 1]) -> Self {
        match value {
            0 => MasterKeyState::Erased,
            1 => MasterKeyState::Onboarding,
            2 => MasterKeyState::Normal,
            _ => MasterKeyState::Unknown,
        }
    }
}

impl AsScalar<1> for MasterKeyState {
    fn as_scalar(&self) -> [u32; 1] {
        [match self {
            MasterKeyState::Erased => 0,
            MasterKeyState::Onboarding => 1,
            MasterKeyState::Normal => 2,
            MasterKeyState::Unknown => 3,
        }]
    }
}
