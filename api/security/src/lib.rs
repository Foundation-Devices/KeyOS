// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod messages;
use {
    bip39::{Error as Bip39Error, Language, Mnemonic},
    crypto::error::CryptoError,
    messages::*,
    std::{num::ParseIntError, str::Utf8Error},
    zeroize::ZeroizeOnDrop,
};

pub const MAX_LOGIN_ATTEMPTS: u32 = 10;
pub const MIN_PIN_LENGTH: usize = 6;

/// FIDO attestation private key for software signing.
/// Corresponding pubkey for testing:
/// 044c0fef3ee1ac94a1cb113e87db62ba64ac3666cce5690c333c7f801d7d4254f1dcc700b76d2ce311170bf543967f4e6b8204cb9ba99f44d3039ee76d1d527560
pub const DEV_FIDO_ATTESTATION_PRIVATE_KEY: [u8; 32] = [
    0xbc, 0x2a, 0x1b, 0xfb, 0xce, 0xf4, 0xf7, 0x53, 0xb8, 0x6e, 0xbe, 0x13, 0x02, 0x13, 0x33, 0xc9, 0xbe,
    0x7e, 0x4c, 0xd0, 0x7b, 0x2a, 0xb9, 0x94, 0xb4, 0xcf, 0x23, 0x36, 0x4b, 0x6f, 0x3c, 0x33,
];

#[derive(Default)]
pub struct Security<P: server::CheckedPermissions> {
    conn: server::CheckedConn<P>,
}

#[derive(Debug, Clone, ZeroizeOnDrop, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct Pin(pub [u8; 32]);

#[derive(Debug, Clone, ZeroizeOnDrop, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum Seed {
    /// Twelve word seed.
    Twelve([u8; 16]),
    /// Twenty-four word seed.
    TwentyFour([u8; 32]),
}

impl Seed {
    /// Creates a new `Seed` from a byte slice. The slice must be either 16 bytes (for a 12-word seed) or 32
    /// bytes (for a 24-word seed).
    ///
    /// # Panics
    ///
    /// Panics if the length of the slice is not 16 or 32 bytes.
    pub fn from_bytes(seed: &[u8]) -> Self {
        match seed.len() {
            16 => Seed::Twelve(seed.try_into().unwrap()),
            32 => Seed::TwentyFour(seed.try_into().unwrap()),
            _ => panic!("Invalid seed length: expected 16 or 32 bytes, got {}", seed.len()),
        }
    }

    pub fn bytes(&self) -> &[u8] {
        match self {
            Seed::Twelve(bytes) => bytes,
            Seed::TwentyFour(bytes) => bytes,
        }
    }

    pub fn to_vec(&self) -> Vec<u8> { self.bytes().to_vec() }

    pub fn from_mnemonic(mnemonic: &Mnemonic) -> Self {
        let entropy = mnemonic.to_entropy();
        Self::from_bytes(&entropy)
    }

    pub fn to_mnemonic(&self) -> Result<Mnemonic, Bip39Error> { Mnemonic::from_entropy(self.bytes()) }

    pub fn to_mnemonic_words(&self) -> Result<Vec<String>, Bip39Error> {
        let mnemonic = self.to_mnemonic()?;
        Ok(mnemonic.words().map(str::to_string).collect())
    }

    pub fn to_standard_seed_qr_data(&self) -> Result<Vec<u8>, Bip39Error> {
        let mnemonic = self.to_mnemonic()?;
        let indices: String = mnemonic.word_indices().map(|idx| format!("{idx:04}")).collect();
        Ok(indices.into_bytes())
    }

    pub fn to_compact_seed_qr_data(&self) -> Result<Vec<u8>, Bip39Error> {
        let mnemonic = self.to_mnemonic()?;
        Ok(mnemonic.to_entropy())
    }
}

impl Default for Seed {
    fn default() -> Self { Seed::TwentyFour([0; 32]) }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ParseSeedQrError {
    #[error("Invalid UTF-8 in word index: {0}")]
    InvalidUtf8(#[from] Utf8Error),

    #[error("Failed to parse word index: {0}")]
    InvalidWordIndex(#[from] ParseIntError),

    #[error("Word index {0} out of range")]
    WordIndexOutOfRange(usize),

    #[error("Invalid mnemonic: {0}")]
    InvalidMnemonic(#[from] Bip39Error),
}

/// Parse standard, compact, or plaintext mnemonic SeedQR format.
/// https://github.com/SeedSigner/seedsigner/blob/dev/docs/seed_qr/README.md
pub fn parse_seedqr(qr_data: &[u8]) -> Result<Mnemonic, ParseSeedQrError> {
    // 12 or 24 word standard qr
    if qr_data.len() == 48 || qr_data.len() == 96 {
        let words = qr_data
            .chunks(4)
            .map(|index| -> Result<&'static str, ParseSeedQrError> {
                let index_str = std::str::from_utf8(index)?;
                let index: usize = index_str.parse()?;
                let word = Language::English
                    .word_list()
                    .get(index)
                    .copied()
                    .ok_or(ParseSeedQrError::WordIndexOutOfRange(index))?;
                Ok(word)
            })
            .collect::<Result<Vec<&'static str>, _>>()?
            .join(" ");

        return Mnemonic::parse(words.as_str()).map_err(Into::into);
    }

    if let Ok(text) = std::str::from_utf8(qr_data) {
        if let Ok(mnemonic) = Mnemonic::parse_normalized(text) {
            return Ok(mnemonic);
        }
    }

    Mnemonic::from_entropy(qr_data).map_err(Into::into)
}

#[derive(Debug, Default, Copy, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, PartialEq, Eq)]
pub enum PinEntryMode {
    #[default]
    Pin = 0,
    Passphrase = 1,
}

impl From<u8> for PinEntryMode {
    fn from(value: u8) -> Self {
        match value {
            0 => PinEntryMode::Pin,
            1 => PinEntryMode::Passphrase,
            _ => PinEntryMode::Pin,
        }
    }
}

impl From<PinEntryMode> for u8 {
    fn from(mode: PinEntryMode) -> u8 { mode as u8 }
}

/// Determines what data apart from the seed the lockout will erase.
/// The seed is always erased.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, PartialEq, Eq)]
pub struct LockoutOptions {
    pub seed_fingerprint: bool,
    pub aes_keys: bool,
}

impl LockoutOptions {
    pub const fn erase_seed_only() -> Self { Self { seed_fingerprint: false, aes_keys: false } }

    pub const fn erase_all() -> Self { Self { seed_fingerprint: true, aes_keys: true } }

    pub const fn erase_aes_keys() -> Self { Self { seed_fingerprint: false, aes_keys: true } }
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, PartialEq, Eq)]
pub struct FirmwareTimestamp(pub [u8; 4]);

impl From<FirmwareTimestamp> for u32 {
    fn from(ts: FirmwareTimestamp) -> u32 { u32::from_le_bytes(ts.0) }
}

impl From<u32> for FirmwareTimestamp {
    fn from(ts: u32) -> FirmwareTimestamp { FirmwareTimestamp(ts.to_le_bytes()) }
}

impl Default for FirmwareTimestamp {
    fn default() -> Self { 0u32.into() }
}

pub struct LastSuccess {
    pub num_fails: u32,
    pub attempts_left: u32,
}

#[macro_export]
macro_rules! use_api {
    () => {
        mod security_permissions {
            use security::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/security"]
            pub struct SecurityPermissions;
        }
        type Security = security::Security<security_permissions::SecurityPermissions>;
    };
}

impl<P: server::CheckedPermissions> Security<P> {
    /// User does not need to be logged in. Use this when setting the seed and PIN for the first
    /// time.
    pub fn set_seed_and_pin(&self, seed: Seed, pin: String, pin_entry: PinEntryMode) -> Result<(), PinError>
    where
        P: server::MessageAllowed<SetSeedAndPin>,
    {
        self.conn.send_blocking_archive(SetSeedAndPin { seed, pin: RawPin(pin), pin_entry })
    }

    /// User must be [logged in](Login) to set a new pin.
    pub fn change_pin(
        &self,
        raw_pin: String,
        seed: Option<Seed>,
        pin_entry: PinEntryMode,
    ) -> Result<(), PinError>
    where
        P: server::MessageAllowed<ChangePin>,
    {
        self.conn.send_blocking_archive(ChangePin { pin: RawPin(raw_pin), seed, pin_entry })
    }

    pub fn is_pin_set(&self) -> Result<bool, AccessDenied>
    where
        P: server::MessageAllowed<IsPinSet>,
    {
        self.conn.send_blocking_archive(IsPinSet)
    }

    pub fn get_pin_entry_mode(&self) -> PinEntryMode
    where
        P: server::MessageAllowed<GetPinEntryMode>,
    {
        self.conn.send_blocking_archive(GetPinEntryMode)
    }

    pub fn log_in(&self, pin: String) -> Result<(), LoginFailed>
    where
        P: server::MessageAllowed<Login>,
    {
        self.conn.send_blocking_archive(Login { pin: RawPin(pin) })
    }

    pub fn log_out(&self)
    where
        P: server::MessageAllowed<Logout>,
    {
        self.conn.send_blocking_scalar(Logout)
    }

    pub fn logged_in(&self) -> bool
    where
        P: server::MessageAllowed<LoggedIn>,
    {
        self.conn.send_blocking_scalar(LoggedIn)
    }

    pub fn attempts_remaining(&self) -> Result<u32, AccessDenied>
    where
        P: server::MessageAllowed<GetAttemptsRemaining>,
    {
        self.conn.send_blocking_archive(GetAttemptsRemaining)
    }

    pub fn factory_reset_counter(&self) -> Result<u32, AccessDenied>
    where
        P: server::MessageAllowed<GetFactoryResetCounter>,
    {
        self.conn.send_blocking_archive(GetFactoryResetCounter)
    }

    /// Fetches the [Seed] from SE.
    ///
    /// # Returns
    ///
    /// - `None` if `otp_key` field of SECURAM is set to all zeros.
    /// - `Some(seed)` otherwise.
    pub fn seed(&self) -> Result<Option<Seed>, AccessDenied>
    where
        P: server::MessageAllowed<GetSeed>,
    {
        self.conn.send_blocking_archive(GetSeed)
    }

    /// User must be [logged in](Login) to change the seed. This is because a XOR operation will
    /// be performed between the seed and the PIN hash before storing it in the SE.
    ///
    /// In case the user is setting the seed for the first time, use [`SetSeedAndPin`] instead.
    pub fn set_seed(&self, seed: Seed) -> Result<(), AccessDenied>
    where
        P: server::MessageAllowed<SetSeed>,
    {
        self.conn.send_blocking_archive(SetSeed(seed))
    }

    pub fn app_seed(&self) -> Result<[u8; 32], AccessDenied>
    where
        P: server::MessageAllowed<GetAppSeed>,
    {
        self.conn.send_blocking_archive(GetAppSeed)
    }

    pub fn lockout(&self, lockout_options: LockoutOptions) -> Result<(), AccessDenied>
    where
        P: server::MessageAllowed<Lockout>,
    {
        self.conn.send_blocking_archive(Lockout { lockout_options, reboot: true })
    }

    pub fn sign_with_security_check_key(&self, data: [u8; 32]) -> Result<[u8; 64], AccessDenied>
    where
        P: server::MessageAllowed<SignWithSecurityCheckKey>,
    {
        self.conn.send_blocking_archive(SignWithSecurityCheckKey(data))
    }

    pub fn sign_with_fido_key(&self, data: [u8; 32]) -> Result<[u8; 64], AccessDenied>
    where
        P: server::MessageAllowed<SignWithFidoKey>,
    {
        self.conn.send_blocking_archive(SignWithFidoKey(data))
    }

    pub fn get_fido_pubkey(&self) -> Result<[u8; 64], AccessDenied>
    where
        P: server::MessageAllowed<GetFidoPubkey>,
    {
        self.conn.send_blocking_archive(GetFidoPubkey)
    }

    pub fn security_words(&self, pin_prefix: &str) -> Result<[SecurityWord; 2], AccessDenied>
    where
        P: server::MessageAllowed<GetSecurityWords>,
    {
        self.conn.send_blocking_archive(GetSecurityWords { pin_prefix: pin_prefix.as_bytes().to_vec() })
    }

    pub fn firmware_timestamp(&self) -> Result<FirmwareTimestamp, AccessDenied>
    where
        P: server::MessageAllowed<GetFirmwareTimestamp>,
    {
        self.conn.send_blocking_archive(GetFirmwareTimestamp)
    }

    pub fn set_firmware_timestamp(&self, timestamp: FirmwareTimestamp) -> Result<(), AccessDenied>
    where
        P: server::MessageAllowed<SetFirmwareTimestamp>,
    {
        self.conn.send_blocking_archive(SetFirmwareTimestamp(timestamp))
    }

    pub fn seed_fingerprint(&self) -> Result<[u8; 32], AccessDenied>
    where
        P: server::MessageAllowed<GetSeedFingerprint>,
    {
        self.conn.send_blocking_archive(GetSeedFingerprint)
    }

    pub fn fingerprint(&self, seed: &Seed) -> Result<[u8; 32], AccessDenied>
    where
        P: server::MessageAllowed<ComputeSeedFingerprint>,
    {
        self.conn.send_blocking_archive(ComputeSeedFingerprint(seed.clone()))
    }

    pub fn os_version_info(&self) -> Result<Option<OsVersionInfo>, AccessDenied>
    where
        P: server::MessageAllowed<GetOsVersionInfo>,
    {
        self.conn.send_blocking_archive(GetOsVersionInfo)
    }

    pub fn bootloader_build_date(&self) -> Result<Option<u64>, AccessDenied>
    where
        P: server::MessageAllowed<GetBootloaderBuildDate>,
    {
        self.conn.send_blocking_archive(GetBootloaderBuildDate)
    }

    pub fn sc_challenge(&self, challenge: [u8; ScChallenge::SIZE]) -> Result<ScProof, ScChallengeError>
    where
        P: server::MessageAllowed<ScChallenge>,
    {
        self.conn.send_blocking_archive(ScChallenge(challenge))
    }

    pub fn device_id(&self) -> Result<DeviceId, GetDeviceIdError>
    where
        P: server::MessageAllowed<GetDeviceId>,
    {
        self.conn.send_blocking_archive(GetDeviceId)
    }

    pub fn get_random(&self) -> Result<[u8; 32], AccessDenied>
    where
        P: server::MessageAllowed<GetRandom>,
    {
        self.conn.send_blocking_archive(GetRandom)
    }

    pub fn keycard_authenticity_mac(&self, msg: [u8; 32]) -> Result<[u8; 32], AccessDenied>
    where
        P: server::MessageAllowed<KeycardAuthenticityMac>,
    {
        self.conn.send_blocking_archive(KeycardAuthenticityMac(msg))
    }

    #[cfg(not(keyos))]
    pub fn get_pin(&self) -> String
    where
        P: server::MessageAllowed<GetPin>,
    {
        self.conn.send_blocking_archive(GetPin)
    }

    #[cfg(not(keyos))]
    pub fn set_attempts_remaining(&self, attempts: u32) -> Result<(), SecurityError>
    where
        P: server::MessageAllowed<SetAttempts>,
    {
        if attempts > MAX_LOGIN_ATTEMPTS {
            return Err(SecurityError::AttemptsOutOfBounds(attempts));
        }

        self.conn.send_blocking_archive(SetAttempts(MAX_LOGIN_ATTEMPTS - attempts));
        Ok(())
    }

    /// Get the bluetooth HMAC challenge secret and whether it was shared with the BT chip already.
    pub fn bluetooth_challenge_secret(&self) -> BluetoothChallengeSecret
    where
        P: server::MessageAllowed<GetBluetoothChallengeSecret>,
    {
        self.conn.send_blocking_archive(GetBluetoothChallengeSecret)
    }

    pub fn set_bluetooth_challenge_secret_sent(&self)
    where
        P: server::MessageAllowed<SetBluetoothCheckSecretSent>,
    {
        self.conn.send_blocking_scalar(SetBluetoothCheckSecretSent)
    }

    pub fn set_bluetooth_device_id(&self, device_id: [u8; 8])
    where
        P: server::MessageAllowed<SetBluetoothDeviceId>,
    {
        self.conn.send_blocking_archive(SetBluetoothDeviceId(device_id))
    }

    pub fn master_key_state(&self) -> MasterKeyState
    where
        P: server::MessageAllowed<GetMasterKeyState>,
    {
        self.conn.send_blocking_scalar(GetMasterKeyState)
    }

    /// Subscribe to the `DiskEncryptionKeysReady` event. The event fires once, when the security server
    /// has written disk encryption keys into SECURAM. Subscribers that arrive after the event has already
    /// fired receive it immediately on subscription.
    pub fn subscribe_disk_encryption_keys_ready<SR>(&self, context: &mut server::ServerContext<SR>)
    where
        P: server::MessageAllowed<SubscribeDiskEncryptionKeysReady>,
        SR: server::ScalarEventHandler<DiskEncryptionKeysReady>,
    {
        self.conn.subscribe_scalar_infallible(SubscribeDiskEncryptionKeysReady, context)
    }
}

/// The state of the master key determined by the combination of the secrets available to the security server.
#[derive(Debug, Copy, Clone)]
pub enum MasterKeyState {
    Onboarding,
    Erased,
    Normal,
    Unknown,
}

#[cfg(not(keyos))]
#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("Attempts remaining must not be greater than max attempts of {}: {0:?}", MAX_LOGIN_ATTEMPTS)]
    AttemptsOutOfBounds(u32),
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SecurityWord(pub usize);

impl std::fmt::Display for SecurityWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        bip39::Language::English.word_list()[self.0].fmt(f)
    }
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, thiserror::Error)]
pub struct AccessDenied;

impl std::fmt::Display for AccessDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "Access denied") }
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, thiserror::Error)]
pub enum PinError {
    #[error("Access denied")]
    AccessDenied,
    #[error("PIN too short")]
    TooShort,
}

impl From<AccessDenied> for PinError {
    fn from(_: AccessDenied) -> Self { PinError::AccessDenied }
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct LoginFailed {
    pub attempts_left: u32,
}

impl std::fmt::Display for LoginFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "Login failed") }
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct OsVersionInfo {
    pub bootloader_version: [u8; 8],
    pub keyos_version: [u8; 20],
}

/// A message sent from the device to the server, serving to prove that the device knows the private key
/// corresponding to the public key it claims to own. The message has the following binary format:
/// ```text
/// ----------------------------------------------------------------------------------------
/// | challenge | deadline | device pubkey | device nonce | bootloader version | signature |
/// | 32 bytes  | 8 bytes  | 33 bytes      | 32 bytes     | 20 bytes           | 64 bytes  |
/// ----------------------------------------------------------------------------------------
/// ```
#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ScProof(pub [u8; Self::SIZE]);

impl ScProof {
    pub const SIZE: usize = 189;
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[repr(u8)]
pub enum ScError {
    Ok = 0,
    InvalidMessageLength = 1,
    InvalidSignature = 3,
    DeadlineExpired = 4,
    UnknownChallenge = 6,
    InvalidBootloaderVersion = 7,
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum ScChallengeError {
    Sc(ScError),
    CryptoAuthLib(i32),
    Crypto(CryptoError),
    AccessDenied,
    Internal(String),
}

#[derive(Debug, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum GetDeviceIdError {
    #[error("crypto auth lib error: {0}")]
    CryptoAuthLib(i32),
    #[error(transparent)]
    Crypto(CryptoError),
    #[error("no bluetooth serial yet")]
    NoBluetoothSerialYet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DeviceId(pub [u8; 32]);

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5], self.0[6], self.0[7]
        )
    }
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BluetoothChallengeSecret {
    pub secret: [u8; 32],
    pub sent: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seed_mnemonic_roundtrip() {
        let seed = Seed::Twelve([0x7Au8; 16]);
        let mnemonic = seed.to_mnemonic().unwrap();
        let recovered_seed = Seed::from_mnemonic(&mnemonic).to_vec();

        assert_eq!(
            &seed.bytes()[..mnemonic.to_entropy().len()],
            &recovered_seed[..mnemonic.to_entropy().len()]
        );
    }

    #[test]
    fn test_parse_seedqr_standard_12_word() {
        // Standard SeedQR format: 48 bytes (12 words * 4)
        let qr_data = b"192402220235174306311124037817700641198012901210";

        let result = parse_seedqr(qr_data).unwrap().word_indices().collect::<Vec<_>>();

        let expected = vec![1924, 222, 235, 1743, 631, 1124, 378, 1770, 641, 1980, 1290, 1210];
        assert_eq!(result, expected, "Word indices should match expected values");
    }

    #[test]
    fn test_parse_seedqr_standard_24_word() {
        let entropy = [0x35u8; 32];
        let mnemonic = Mnemonic::from_entropy(&entropy).unwrap();

        let indices: String = mnemonic.word_indices().map(|idx| format!("{idx:04}")).collect();
        let qr_data = indices.as_bytes();
        let result = parse_seedqr(qr_data).unwrap();

        assert_eq!(result, mnemonic);
        assert_eq!(result.word_count(), 24);
    }

    #[test]
    fn test_parse_seedqr_compact() {
        fn test(entropy: &[u8]) {
            let mnemonic = Mnemonic::from_entropy(entropy).unwrap();
            let result = parse_seedqr(entropy).unwrap();
            assert_eq!(result, mnemonic);
        }

        test(&[0x11u8; 16]);
        test(&[0x22u8; 32]);
    }

    #[test]
    fn test_parse_seedqr_plaintext() {
        let mnemonic = Mnemonic::from_entropy(&[0x5Au8; 16]).unwrap();
        let qr_data = mnemonic.to_string();
        let result = parse_seedqr(qr_data.as_bytes()).unwrap();

        assert_eq!(result, mnemonic);
    }

    #[test]
    fn test_parse_seedqr_plaintext_with_extra_whitespace() {
        let mnemonic = Mnemonic::from_entropy(&[0xA5u8; 32]).unwrap();
        let words = mnemonic.words().collect::<Vec<_>>();
        let qr_data = format!("  {}\n{}\n  ", words[..12].join("  "), words[12..].join("\n"));

        let result = parse_seedqr(qr_data.as_bytes()).unwrap();
        assert_eq!(result, mnemonic);
    }

    #[test]
    fn test_seedqr_generation_roundtrip() {
        let seed = Seed::Twelve([0x6Cu8; 16]);

        let standard_data = seed.to_standard_seed_qr_data().unwrap();
        let parsed_standard = parse_seedqr(&standard_data).unwrap();
        let recovered_seed = Seed::from_mnemonic(&parsed_standard).to_vec();
        assert_eq!(
            &seed.bytes()[..parsed_standard.to_entropy().len()],
            &recovered_seed[..parsed_standard.to_entropy().len()]
        );

        let compact_data = seed.to_compact_seed_qr_data().unwrap();
        let parsed_compact = parse_seedqr(&compact_data).unwrap();
        let recovered_seed = Seed::from_mnemonic(&parsed_compact).to_vec();
        assert_eq!(
            &seed.bytes()[..parsed_compact.to_entropy().len()],
            &recovered_seed[..parsed_compact.to_entropy().len()]
        );
    }

    #[test]
    fn test_parse_seedqr_errors() {
        // Invalid UTF-8 in standard format (48 bytes)
        let invalid_utf8 = vec![0xFF; 48];
        assert!(matches!(parse_seedqr(&invalid_utf8), Err(ParseSeedQrError::InvalidUtf8(_))));

        // Invalid number format in standard format
        let invalid_number = b"abcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd"; // 48 bytes
        assert!(matches!(parse_seedqr(invalid_number), Err(ParseSeedQrError::InvalidWordIndex(_))));

        // Out of range index
        let out_of_range = b"999999999999999999999999999999999999999999999999"; // 48 bytes
        assert!(matches!(parse_seedqr(out_of_range), Err(ParseSeedQrError::WordIndexOutOfRange(9999))));

        // Invalid compact format (not a valid entropy length)
        let invalid_compact = b"invalid"; // 7 bytes
        assert!(matches!(parse_seedqr(invalid_compact), Err(ParseSeedQrError::InvalidMnemonic(_))));
    }
}
