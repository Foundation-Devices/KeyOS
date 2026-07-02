#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "semver")]
use core::str::FromStr;

use chrono::Datelike;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trust {
    /// only signed by a developper key
    ThirdParty,
    /// trust is disabled by the verifier
    Disabled,
    /// only signed by a key declared as trusted, but unverified yet
    Unverified,
    /// only signed by a single verified trusted key
    PartiallyTrusted,
    /// fully signed by two different verified trusted keys
    FullyTrusted,
}

#[derive(Debug, Clone)]
pub struct Header {
    magic: [u8; 4],
    timestamp: [u8; 4],
    date: [u8; 14],
    version: [u8; 20],
    bin_size: [u8; 4],
    pubkey1: [u8; 33],
    signature1: [u8; 64],
    pubkey2: [u8; 33],
    signature2: [u8; 64],

    /// Size of the header in bytes (not present in the header).
    size: usize,
    /// Hash of the header and binary code (not present in the header).
    hash: [u8; 32],
    /// Hash of the binary only (not present in the header).
    bin_hash: [u8; 32],
    /// State of trust according to included signatures (not present in the header).
    trust: Trust,
}

/// SHA-256 hash function.
pub trait Sha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32];
}

#[cfg(feature = "std")]
pub trait Sha256Streaming {
    type Error;

    /// `total_len`: total length of data to hash
    /// `reader`: reader that provides the data to hash (e.g., file)
    fn hash_streaming<R: std::io::Read>(&self, total_len: usize, reader: R) -> Result<[u8; 32], Self::Error>;
}

/// ECDSA secp256k1 signing.
pub trait Secp256k1Sign {
    /// Sign a message on the secp256k1 curve.
    fn sign_ecdsa(&self, msg: [u8; 32]) -> [u8; 64];

    /// Get the public key used for signing.
    fn pubkey(&self) -> [u8; 33];
}

/// ECDSA secp256k1 verification.
pub trait Secp256k1Verify {
    /// Verify an ECDSA signature against the given public key.
    fn verify_ecdsa(&self, msg: [u8; 32], signature: [u8; 64], pubkey: [u8; 33]) -> VerificationResult;
}

/// Verification result.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub enum VerificationResult {
    // The values are arbitrary, but chosen to be different by more than one bit to make glitching
    // attacks more difficult.
    Valid = 0xcafebabe,
    Invalid = 0xdeadbeef,
}

impl Header {
    /// Default size of the header in bytes.
    pub const DEFAULT_SIZE: usize = 2048;
    /// Maximum size of the header in bytes.
    pub const MAX_SIZE: usize = 4096;
    /// Minimum size of the header in bytes.
    pub const MIN_SIZE: usize = 256;

    /// Magic number.
    pub fn magic(&self) -> Magic { Magic::from_bytes(self.magic).expect("validated") }

    /// Binary timestamp in seconds since the Unix epoch.
    pub fn timestamp(&self) -> u32 { u32::from_le_bytes(self.timestamp) }

    /// Human-readable binary date.
    ///
    /// Used for displaying the binary date during device boot, where advanced
    /// date manipulation functions are not available.
    pub fn date(&self) -> &str {
        let first_zero = self.date.iter().position(|&b| b == 0).unwrap_or(self.date.len());
        core::str::from_utf8(&self.date[..first_zero]).expect("validated")
    }

    /// Binary version.
    pub fn version(&self) -> &str {
        let first_zero = self.version.iter().position(|&b| b == 0).unwrap_or(self.version.len());
        core::str::from_utf8(&self.version[..first_zero]).expect("validated")
    }

    /// The size of the binary.
    pub fn bin_size(&self) -> u32 { u32::from_le_bytes(self.bin_size) }

    /// Public key of the first signer.
    pub fn pubkey1(&self) -> [u8; 33] { self.pubkey1 }

    /// First signature.
    pub fn signature1(&self) -> [u8; 64] { self.signature1 }

    /// Public key of the second signer.
    pub fn pubkey2(&self) -> [u8; 33] { self.pubkey2 }

    /// Second signature.
    pub fn signature2(&self) -> [u8; 64] { self.signature2 }

    /// Hash of the binary that this header is for.
    ///
    /// All zeros if the header was created using
    /// [`parse_unverified`](Header::parse_unverified).
    pub fn binary_hash(&self) -> &[u8; 32] { &self.bin_hash }

    /// State of trust according to included signatures.
    pub fn trust(&self) -> Trust { self.trust }

    /// Create a new header and sign it.
    pub fn sign_new(
        magic: Magic,
        version: &str,
        timestamp: u32,
        signer: Signer,
        binary: &[u8],
        sha: &impl Sha256,
        secp: &impl Secp256k1Sign,
        header_size: usize,
    ) -> Result<Self, Error> {
        // Validate the version string.
        #[cfg(feature = "semver")]
        semver::Version::from_str(version).map_err(|_| Error::InvalidVersionSemver)?;

        if header_size > Self::MAX_SIZE {
            return Err(Error::HeaderTooLong);
        }

        let mut header = Self {
            magic: magic.to_bytes(),
            timestamp: timestamp.to_le_bytes(),
            date: [0; 14],
            version: [0; 20],
            bin_size: u32::try_from(binary.len()).map_err(|_| Error::BinaryTooLong)?.to_le_bytes(),
            pubkey1: [0; 33],
            signature1: [0; 64],
            pubkey2: [0; 33],
            signature2: [0; 64],
            size: header_size,
            hash: [0; 32],
            bin_hash: [0; 32],
            trust: Trust::Unverified,
        };
        header.set_date(timestamp);
        header.set_version(version)?;
        let reserved = [0; Self::MAX_SIZE];
        header.hash(&reserved[..header_size - header.payload_size()], binary, sha);
        header.validate_fields(binary, true)?;

        // Sign the header.
        match signer {
            Signer::Trusted => {
                // Trusted key is used, so both first and second signatures need to be filled
                // in. This key is used for the first signature.
                header.pubkey1 = secp.pubkey();
                header.signature1 = secp.sign_ecdsa(header.hash);
            }
            Signer::Developer => {
                // Developer key is used, so only the second signature is filled in.
                // The first signature is left empty (zeroed out).
                header.pubkey2 = secp.pubkey();
                header.signature2 = secp.sign_ecdsa(header.hash);
                header.trust = Trust::ThirdParty;
            }
        }

        Ok(header)
    }

    /// Parse a header.
    ///
    /// Verifies that any existing signatures are signed by the given known
    /// signers. If the known signers slice is empty, any signer is
    /// accepted.
    ///
    /// If the header is missing, `None` is returned.
    pub fn parse(
        data: &[u8],
        known_signers: &[[u8; 33]],
        sha: &impl Sha256,
        secp: &impl Secp256k1Verify,
        header_size: usize,
    ) -> Result<Option<Self>, Error> {
        if header_size > Self::MAX_SIZE {
            return Err(Error::HeaderTooLong);
        }

        let Some(mut header) = Header::deserialize(data, header_size)? else {
            return Ok(None);
        };

        // When parsing an existing header, there should always be at least one
        // signature.
        if header.signature1 == [0; 64] && header.signature2 == [0; 64] {
            return Err(Error::HeaderWithNoSignature);
        }

        let reserved = &data[header.payload_size()..header_size];
        let binary = &data[header_size..];
        header.hash(reserved, binary, sha);
        header.verify_signatures(known_signers, secp)?;
        header.update_trust(known_signers)?;
        header.validate_fields(binary, true)?;

        Ok(Some(header))
    }

    /// Reads the header without verifying the signatures. Be careful with
    /// trusting the unverified data.
    ///
    /// Use the [`parse`](Header::parse) method to read and verify the header
    /// signatures.
    pub fn parse_unverified(
        data: &[u8],
        header_size: usize,
        check_size: bool,
    ) -> Result<Option<Self>, Error> {
        if header_size > Self::MAX_SIZE {
            return Err(Error::HeaderTooLong);
        }

        let Some(header) = Self::deserialize(data, header_size)? else {
            return Ok(None);
        };
        match header.validate_fields(&data[header.size..], check_size) {
            Ok(()) => Ok(Some(header)),
            Err(e) => Err(e),
        }
    }

    /// Parse a header with streaming hash computation
    ///
    /// This is a memory-efficient version of [`parse`](Header::parse) that uses
    /// streaming SHA-256 to hash the binary data without loading it entirely into memory
    #[cfg(feature = "std")]
    pub fn parse_streaming<E, R>(
        header_data: &[u8],
        binary_size: usize,
        known_signers: &[[u8; 33]],
        sha: &impl Sha256,
        sha_streaming: &impl Sha256Streaming<Error = E>,
        secp: &impl Secp256k1Verify,
        header_size: usize,
        binary_reader: R,
    ) -> Result<Option<Self>, Error>
    where
        E: core::fmt::Debug,
        R: std::io::Read,
    {
        if header_size > Self::MAX_SIZE {
            return Err(Error::HeaderTooLong);
        }
        if header_data.len() < header_size {
            return Err(Error::HeaderTooShort);
        }

        let Some(mut header) = Header::deserialize(header_data, header_size)? else {
            return Ok(None);
        };

        // When parsing an existing header, there should always be at least one
        // signature.
        if header.signature1 == [0; 64] && header.signature2 == [0; 64] {
            return Err(Error::HeaderWithNoSignature);
        }

        // Validate binary size matches header
        if header.bin_size() as usize != binary_size {
            return Err(Error::InvalidSize);
        }

        let reserved = &header_data[header.payload_size()..header_size];

        // Hash the binary with a streaming SHA-256
        header.hash_streaming(reserved, binary_size, sha, sha_streaming, binary_reader)
            .map_err(|_| Error::HashError)?;

        header.verify_signatures(known_signers, secp)?;
        header.update_trust(known_signers)?;

        // Use validate_fields with check_size: `false` since we already validated
        // binary size above and don't have the binary slice in memory
        header.validate_fields(&[], false)?;

        Ok(Some(header))
    }

    /// Serialize the header to a buffer. Exactly [`self.size`] bytes will be
    /// written.
    pub fn serialize(&self, buf: &mut [u8]) -> Result<(), Error> {
        if buf.len() < self.size {
            return Err(Error::SerializeBufferTooSmall);
        }

        buf[..4].copy_from_slice(&self.magic);
        buf[4..8].copy_from_slice(&self.timestamp);
        buf[8..22].copy_from_slice(&self.date);
        buf[22..42].copy_from_slice(&self.version);
        buf[42..46].copy_from_slice(&self.bin_size);
        buf[46..79].copy_from_slice(&self.pubkey1);
        buf[79..143].copy_from_slice(&self.signature1);
        buf[143..176].copy_from_slice(&self.pubkey2);
        buf[176..240].copy_from_slice(&self.signature2);
        buf[240..self.size].fill(0);

        Ok(())
    }

    /// Add a second signature to the header.
    ///
    /// The first signature must be present, and the second signature must be
    /// missing, otherwise an error is returned.
    pub fn add_second_signature(&mut self, secp: &impl Secp256k1Sign) -> Result<(), Error> {
        if self.signature2 != [0; 64] {
            return Err(Error::Signature2Present);
        }
        if self.signature1 == [0; 64] {
            return Err(Error::Signature1Missing);
        }
        let pubkey = secp.pubkey();
        if self.pubkey1 == pubkey {
            return Err(Error::PubkeyAlreadyUsed);
        }
        self.pubkey2 = pubkey;
        self.signature2 = secp.sign_ecdsa(self.hash);
        Ok(())
    }

    /// Returns the size of the actual header data in bytes (excuding the reserved padding).
    #[inline]
    const fn payload_size(&self) -> usize {
        self.magic.len()
            + self.timestamp.len()
            + self.date.len()
            + self.version.len()
            + self.bin_size.len()
            + self.pubkey1.len()
            + self.signature1.len()
            + self.pubkey2.len()
            + self.signature2.len()
    }

    /// Update the trust status according to a list of known trusted signers.
    ///
    /// No assumption is done during this check because the header could have been maliciously manually
    /// modified.
    fn update_trust(&mut self, known_signers: &[[u8; 33]]) -> Result<(), Error> {
        self.trust = if known_signers.len() == 0 {
            Trust::Disabled
        } else {
            match (self.pubkey1, self.pubkey2) {
                (pubkey1, pubkey2)
                    if known_signers.contains(&pubkey1)
                        && known_signers.contains(&pubkey2)
                        && pubkey1 != pubkey2 =>
                {
                    Trust::FullyTrusted
                }
                (pubkey1, pubkey2) if known_signers.contains(&pubkey1) && pubkey2 == [0; 33] => {
                    Trust::PartiallyTrusted
                }
                (pubkey1, pubkey2) if pubkey1 == [0; 33] && !known_signers.contains(&pubkey2) => {
                    Trust::ThirdParty
                }
                _ => return Err(Error::BrokenSignatures),
            }
        };
        Ok(())
    }

    /// Deserialize the header fields from a buffer.
    ///
    /// Returns `None` if the buffer does not contain a header.
    fn deserialize(data: &[u8], header_size: usize) -> Result<Option<Self>, Error> {
        if data.len() < 4 {
            return Ok(None);
        }

        let magic = data[..4].try_into().unwrap();
        if Magic::from_bytes(magic).is_none() {
            // Magic value is missing or not recognized, so this is not a header.
            return Ok(None);
        }

        // The data contains a header, so make sure it's of appropriate length.
        if data.len() < header_size {
            return Err(Error::HeaderTooShort);
        }

        let timestamp = data[4..8].try_into().unwrap();
        let date = data[8..22].try_into().unwrap();
        let version = data[22..42].try_into().unwrap();
        let bin_size = data[42..46].try_into().unwrap();
        let pubkey1 = data[46..79].try_into().unwrap();
        let signature1 = data[79..143].try_into().unwrap();
        let pubkey2 = data[143..176].try_into().unwrap();
        let signature2 = data[176..240].try_into().unwrap();

        Ok(Some(Self {
            size: header_size,
            magic,
            timestamp,
            date,
            version,
            bin_size,
            pubkey1,
            signature1,
            pubkey2,
            signature2,
            hash: [0; 32],
            bin_hash: [0; 32],
            trust: Trust::Unverified,
        }))
    }

    /// Hash up the header fields (magic, timestamp, date, version, bin_size)
    fn compute_header_fields_hash(&self, sha: &impl Sha256) -> [u8; 32] {
        let mut hash_buf = [0; 128];

        // Fill header hash buf with header data
        let mut offset = 0;
        hash_buf[offset..offset + self.magic.len()].copy_from_slice(&self.magic);
        offset += self.magic.len();
        hash_buf[offset..offset + self.timestamp.len()].copy_from_slice(&self.timestamp);
        offset += self.timestamp.len();
        hash_buf[offset..offset + self.date.len()].copy_from_slice(&self.date);
        offset += self.date.len();
        hash_buf[offset..offset + self.version.len()].copy_from_slice(&self.version);
        offset += self.version.len();
        hash_buf[offset..offset + self.bin_size.len()].copy_from_slice(&self.bin_size);
        sha.hash(&hash_buf)
    }

    /// Compute the final hash from the header, reserved, and binary hashes
    /// Sets `self.hash` and `self.bin_hash`
    fn finalize_hash(&mut self, header_hash: [u8; 32], reserved_hash: [u8; 32], bin_hash: [u8; 32], sha: &impl Sha256) {
        let mut hash_buf = [0u8; 128];
        hash_buf[0..32].copy_from_slice(&header_hash);
        hash_buf[32..64].copy_from_slice(&reserved_hash);
        hash_buf[64..96].copy_from_slice(&bin_hash);
        let hash = sha.hash(&hash_buf);

        // Hash twice to prevent length extension attacks
        self.hash = sha.hash(&hash);
        self.bin_hash = bin_hash;
    }

    /// Hash up the header and store it in the `hash` field
    fn hash(&mut self, reserved: &[u8], binary: &[u8], sha: &impl Sha256) {
        let header_hash = self.compute_header_fields_hash(sha);
        let reserved_hash = sha.hash(reserved);
        let bin_hash = sha.hash(binary);
        self.finalize_hash(header_hash, reserved_hash, bin_hash, sha);
    }

    /// Hash up the header using streaming SHA-256 for the binary data
    #[cfg(feature = "std")]
    fn hash_streaming<E, R: std::io::Read>(
        &mut self,
        reserved: &[u8],
        binary_size: usize,
        sha: &impl Sha256,
        sha_streaming: &impl Sha256Streaming<Error = E>,
        binary_reader: R,
    ) -> Result<(), E>
    {
        let header_hash = self.compute_header_fields_hash(sha);
        let reserved_hash = sha.hash(reserved);
        let bin_hash = sha_streaming.hash_streaming(binary_size, binary_reader)?;
        self.finalize_hash(header_hash, reserved_hash, bin_hash, sha);
        Ok(())
    }

    fn verify_signatures(
        &mut self,
        known_signers: &[[u8; 33]],
        secp: &impl Secp256k1Verify,
    ) -> Result<(), Error> {
        if self.signature1 != [0; 64] {
            if secp.verify_ecdsa(self.hash, self.signature1, self.pubkey1) != VerificationResult::Valid {
                return Err(Error::InvalidSignature1);
            }
            // Trusted scheme only
            if !known_signers.is_empty() && !known_signers.contains(&self.pubkey1) {
                return Err(Error::UnknownPubkey1);
            }
        }
        if self.signature2 != [0; 64] {
            if secp.verify_ecdsa(self.hash, self.signature2, self.pubkey2) != VerificationResult::Valid {
                return Err(Error::InvalidSignature2);
            }
            if !known_signers.is_empty()
                && !known_signers.contains(&self.pubkey2)
                && self.signature1 != [0; 64]
            {
                // Trusted scheme only
                return Err(Error::UnknownPubkey2);
            }
        }
        Ok(())
    }

    /// Validate the fields in the header.
    fn validate_fields(&self, binary: &[u8], check_size: bool) -> Result<(), Error> {
        // Validate that the version string is UTF-8 formatted according to SemVer, and
        // that the unused bytes are all zero.
        let first_zero = self.version.iter().position(|&b| b == 0).unwrap_or(self.version.len());
        let _version =
            core::str::from_utf8(&self.version[..first_zero]).map_err(|_| Error::InvalidVersionUtf8)?;
        #[cfg(feature = "semver")]
        semver::Version::from_str(_version).map_err(|_| Error::InvalidVersionSemver)?;
        if self.version[first_zero..].iter().any(|&b| b != 0) {
            return Err(Error::InvalidVersionTrailingBytes);
        }

        // Validate that the date string is UTF-8, and that the unused bytes are all
        // zero.
        let first_zero = self.date.iter().position(|&b| b == 0).unwrap_or(self.date.len());
        core::str::from_utf8(&self.date[..first_zero]).map_err(|_| Error::InvalidDateUtf8)?;
        if self.date[first_zero..].iter().any(|&b| b != 0) {
            return Err(Error::InvalidDateTrailingBytes);
        }

        // Verify that the binary size is correct if requested
        if check_size {
            let bin_size = u32::from_le_bytes(self.bin_size);
            let actual_bin_size = u32::try_from(binary.len()).map_err(|_| Error::BinaryTooLong)?;
            if bin_size != actual_bin_size {
                return Err(Error::InvalidBinarySize { header: bin_size, actual: actual_bin_size });
            }
        }

        // If a signature is zero, the corresponding pubkey must also be zero.
        if self.signature1 == [0; 64] && self.pubkey1 != [0; 33] {
            return Err(Error::InvalidPubkey1);
        }
        if self.signature2 == [0; 64] && self.pubkey2 != [0; 33] {
            return Err(Error::InvalidPubkey2);
        }

        Ok(())
    }

    /// Set the binary date field.
    fn set_date(&mut self, timestamp: u32) {
        let date = chrono::DateTime::from_timestamp(timestamp.into(), 0).expect("before 2106");
        let month_num = date.month();
        let month: &[u8] = match month_num {
            1 => b"Jan",
            2 => b"Feb",
            3 => b"Mar",
            4 => b"Apr",
            5 => b"May",
            6 => b"June",
            7 => b"July",
            8 => b"Aug",
            9 => b"Sep",
            10 => b"Oct",
            11 => b"Nov",
            12 => b"Dec",
            _ => unreachable!(),
        };
        self.date.fill(0);

        let mut offset = 0;
        push_slice(&mut self.date, &mut offset, month);

        // Keep month abbreviations punctuated except months that fit naturally in 3-4 chars
        // without punctuation (May/June/July).
        if !(5..=7).contains(&month_num) {
            push_byte(&mut self.date, &mut offset, b'.');
        }

        push_byte(&mut self.date, &mut offset, b' ');

        let day = date.day();
        if day >= 10 {
            push_byte(&mut self.date, &mut offset, ascii_digit(day / 10));
        }
        push_byte(&mut self.date, &mut offset, ascii_digit(day % 10));

        push_byte(&mut self.date, &mut offset, b',');
        push_byte(&mut self.date, &mut offset, b' ');

        let year: u32 = date.year().try_into().expect("year in timestamp is valid");
        push_byte(&mut self.date, &mut offset, ascii_digit((year / 1000) % 10));
        push_byte(&mut self.date, &mut offset, ascii_digit((year / 100) % 10));
        push_byte(&mut self.date, &mut offset, ascii_digit((year / 10) % 10));
        push_byte(&mut self.date, &mut offset, ascii_digit(year % 10));
    }

    /// Set the binary version field.
    fn set_version(&mut self, version: &str) -> Result<(), Error> {
        if version.len() > self.version.len() {
            return Err(Error::VersionTooLong);
        }
        self.version[..version.len()].copy_from_slice(version.as_bytes());
        Ok(())
    }
}

/// Who is signing the binary.
pub enum Signer {
    /// Signed by a trusted identity from Foundation Devices, Inc.
    ///
    /// Headers signed by trusted keys expect both signatures to be filled in.
    Trusted,
    /// Signed by a third-party developer.
    ///
    /// Headers signed by developer keys expect only the second signature to be
    /// filled in. The first signature is left empty (zeroed out).
    Developer,
}

/// Magic number.
///
/// Used to identify the header, and to differentiate header formats for
/// different devices if needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Magic {
    Atsama5d27KeyOs,
    Nrf52Ble,
}

impl Magic {
    pub fn from_bytes(b: [u8; 4]) -> Option<Self> {
        match b {
            [0x50, 0x52, 0x4D, 0x31] => Some(Self::Atsama5d27KeyOs),
            [0x62, 0x6c, 0x65, 0x66] => Some(Self::Nrf52Ble),
            _ => None,
        }
    }

    pub fn to_bytes(&self) -> [u8; 4] {
        match self {
            Self::Atsama5d27KeyOs => [0x50, 0x52, 0x4D, 0x31],
            Self::Nrf52Ble => [0x62, 0x6c, 0x65, 0x66],
        }
    }
}

fn ascii_digit(b: u32) -> u8 {
    match b {
        0 => b'0',
        1 => b'1',
        2 => b'2',
        3 => b'3',
        4 => b'4',
        5 => b'5',
        6 => b'6',
        7 => b'7',
        8 => b'8',
        9 => b'9',
        _ => unreachable!(),
    }
}

fn push_byte<const N: usize>(buf: &mut [u8; N], offset: &mut usize, b: u8) {
    buf[*offset] = b;
    *offset += 1;
}

fn push_slice<const N: usize>(buf: &mut [u8; N], offset: &mut usize, s: &[u8]) {
    buf[*offset..*offset + s.len()].copy_from_slice(s);
    *offset += s.len();
}

#[derive(Debug)]
pub enum Error {
    BinaryTooLong,
    HeaderTooShort,
    HeaderTooLong,
    HeaderWithNoSignature,
    InvalidDateTrailingBytes,
    InvalidDateUtf8,
    InvalidBinarySize { header: u32, actual: u32 },
    InvalidPubkey1,
    InvalidPubkey2,
    InvalidReservedBytes,
    InvalidSignature1,
    InvalidSignature2,
    InvalidTimestamp,
    InvalidVersionSemver,
    InvalidVersionTrailingBytes,
    InvalidVersionUtf8,
    PubkeyAlreadyUsed,
    SerializeBufferTooSmall,
    Signature1Missing,
    Signature2Present,
    UnknownPubkey1,
    UnknownPubkey2,
    VersionTooLong,
    BrokenSignatures,
    InvalidSize,
    HashError,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BinaryTooLong => write!(f, "binary too long"),
            Self::HeaderTooShort => write!(f, "header too short"),
            Self::HeaderTooLong => write!(f, "header too long"),
            Self::HeaderWithNoSignature => write!(f, "header with no signature"),
            Self::InvalidDateTrailingBytes => write!(f, "invalid date trailing bytes in header"),
            Self::InvalidDateUtf8 => write!(f, "invalid date UTF-8 in header"),
            Self::InvalidBinarySize { header: in_header, actual } => {
                write!(f, "invalid binary size in header: should be {actual}, but is {in_header}",)
            }
            Self::InvalidPubkey1 => write!(f, "invalid pubkey1 in header"),
            Self::InvalidPubkey2 => write!(f, "invalid pubkey2 in header"),
            Self::InvalidReservedBytes => write!(f, "invalid reserved bytes in header"),
            Self::InvalidSignature1 => write!(f, "invalid signature1 in header"),
            Self::InvalidSignature2 => write!(f, "invalid signature2 in header"),
            Self::InvalidTimestamp => write!(f, "invalid timestamp in header"),
            Self::InvalidVersionSemver => write!(f, "invalid version SemVer in header"),
            Self::InvalidVersionTrailingBytes => {
                write!(f, "invalid version trailing bytes in header")
            }
            Self::InvalidVersionUtf8 => write!(f, "invalid version UTF-8 in header"),
            Self::PubkeyAlreadyUsed => write!(f, "attempting to sign with the same pubkey twice"),
            Self::SerializeBufferTooSmall => write!(f, "buffer too small for serialization"),
            Self::Signature1Missing => write!(f, "signature1 missing in header"),
            Self::Signature2Present => {
                write!(f, "signature2 already present in header")
            }
            Self::UnknownPubkey1 => write!(f, "unknown pubkey1 in header"),
            Self::UnknownPubkey2 => write!(f, "unknown pubkey2 in header"),
            Self::VersionTooLong => write!(f, "version too long to write in header"),
            Self::BrokenSignatures => write!(f, "broken signatures scheme in header"),
            Self::InvalidSize => write!(f, "binary size in the header doesn't match actual size"),
            Self::HashError => write!(f, "error during streaming hash computation"),
        }
    }
}
