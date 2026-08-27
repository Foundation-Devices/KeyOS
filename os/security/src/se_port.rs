// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

// Code originally ported from passport2 pins.c file.

use std::fmt;

use constant_time_eq::{constant_time_eq, constant_time_eq_n};
use crypto::{error::CryptoError, SHA256_HASH_SIZE};
use rand::RngCore;
use security::{FirmwareTimestamp, LastSuccess, LockoutOptions, Pin, Seed, MAX_LOGIN_ATTEMPTS};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{
    config::{Counter, Slot},
    seed_fingerprint, sha256_batch, CryptoApi,
};

const OP_GENDIG: u8 = 0x15;
const OP_WRITE: u8 = 0x12;
const OP_MAC: u8 = 0x08;

/// Keycard secret for testing only.
const DEV_KEYCARD_SECRET: [u8; 32] = [
    0x2d, 0x12, 0x61, 0xba, 0x05, 0x7b, 0xfa, 0x10, 0xf2, 0x26, 0x2e, 0x37, 0x50, 0xb7, 0x76, 0x13, 0x0b,
    0xff, 0xc1, 0x7b, 0xcf, 0x7f, 0x5f, 0x2f, 0xb5, 0xcd, 0x2e, 0x70, 0x4c, 0xf8, 0x9c, 0xae,
];

/// SHA256 PIN hash with values mixed in from the SE.
///
/// NOTE: This is not simply a hash of the PIN.
// Don't derive Debug: this authenticates to the SE, so it is as good as the PIN.
#[derive(Clone, ZeroizeOnDrop)]
pub struct AuthPinHash(pub [u8; SHA256_HASH_SIZE]);

/// SHA256 hash of a PIN and an encryption salt.
///
/// Used to perform XOR operations with the seed in order to make sure that the PIN is always required.
// Don't derive Debug: this XORs the stored seed, so leaking it decrypts the seed.
#[derive(Clone, ZeroizeOnDrop)]
pub struct XorPinHash(pub [u8; SHA256_HASH_SIZE]);

impl XorPinHash {
    pub fn new(crypto: &CryptoApi, raw_pin: &str) -> Result<Self, CryptoError> {
        const SEED_ENCRYPTION_SALT: &[u8] = b"SeedEncryption";
        sha256_batch(crypto, &[raw_pin.as_bytes(), SEED_ENCRYPTION_SALT]).map(Self)
    }
}

pub trait SeedExtras {
    fn pin_hash_xor(self, xor_hash: &XorPinHash) -> XorSeed;
}

impl SeedExtras for Seed {
    fn pin_hash_xor(self, xor_hash: &XorPinHash) -> XorSeed {
        match self {
            Seed::Twelve(mut seed) => {
                seed.iter_mut().zip(xor_hash.0).for_each(|(x, y)| *x ^= y);
                XorSeed::Twelve(seed)
            }
            Seed::TwentyFour(mut seed) => {
                seed.iter_mut().zip(xor_hash.0).for_each(|(x, y)| *x ^= y);
                XorSeed::TwentyFour(seed)
            }
        }
    }
}

/// Seed that has already beed XORed with a [XorPinHash]. Only this seed variant should be stored in SE.
///
/// # CAUTION
///
/// Do not store this seed in the SE as is, first encrypt it using
/// [XorSeed::encrypt_tagged].
///
/// # Developer Note
///
/// In order to preserve the knowledge of which seed variant was used, the last byte of the array
/// returned by [XorSeed::encrypt_tagged] is a tag that denotes the length of the seed as:
///
///  - `0x12` for twelve word seed (16 bytes),
///  - `0x24` for twenty four word seed (32 bytes).
///
///  Some of the code related to this would have been simpler if the tag was the first byte, but
///  it would make it impossible to use the [Slot::Seed] contents as a write key since the last
///  byte of the [Seed::TwentyFour] would have been in the next SE data block.
#[derive(Clone, ZeroizeOnDrop)]
pub enum XorSeed {
    Twelve([u8; 16]),
    TwentyFour([u8; 32]),
}

impl fmt::Debug for XorSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Twelve(_) => f.debug_tuple("Twelve").field(&"<redacted>").finish(),
            Self::TwentyFour(_) => f.debug_tuple("TwentyFour").field(&"<redacted>").finish(),
        }
    }
}

impl XorSeed {
    const TWELVE_WORD_TAG: u8 = 0x12;
    const TWENTY_FOUR_WORD_TAG: u8 = 0x24;

    /// Decrypt the seed from the SE.
    ///
    /// The last byte in `tagged_bytes` denotes the length of the seed as:
    ///  - 0x12 for twelve word seed (16 bytes),
    ///  - 0x24 for twenty four word seed (32 bytes).
    fn decrypt_tagged(mut tagged_bytes: [u8; 33], otp_key: &[u8; 72]) -> Self {
        let skip_decrypting = constant_time_eq(&tagged_bytes[..32], &[0; 32]);
        if !skip_decrypting {
            tagged_bytes.iter_mut().zip(otp_key.iter()).for_each(|(s, k)| *s ^= *k);
        }

        let (tag, seed) = tagged_bytes.split_last().expect("array is not empty");

        match *tag {
            Self::TWELVE_WORD_TAG => XorSeed::Twelve(seed[..16].try_into().expect("incorrect slice length")),
            Self::TWENTY_FOUR_WORD_TAG => {
                XorSeed::TwentyFour(seed[..32].try_into().expect("incorrect slice length"))
            }
            _ => panic!("Invalid seed tag: {tag}"),
        }
    }

    /// Encrypt the seed and return a tagged byte array. Use this method
    /// to prepare the seed for writing to the SE.
    ///
    /// The last byte in `bytes_tagged` denotes the length of the seed as:
    ///  - `0x12` for twelve word seed (16 bytes),
    ///  - `0x24` for twenty four word seed (32 bytes).
    fn encrypt_tagged(&self, otp_key: &[u8; 72]) -> [u8; 33] {
        let mut tagged_bytes = [0u8; 33];

        let skip_encrypting = match self {
            XorSeed::Twelve(seed) => {
                tagged_bytes[..16].copy_from_slice(seed);
                tagged_bytes[32] = Self::TWELVE_WORD_TAG;
                constant_time_eq_n(seed, &[0; 16])
            }
            XorSeed::TwentyFour(seed) => {
                tagged_bytes[..32].copy_from_slice(seed);
                tagged_bytes[32] = Self::TWENTY_FOUR_WORD_TAG;
                constant_time_eq_n(seed, &[0; 32])
            }
        };

        if !skip_encrypting {
            tagged_bytes.iter_mut().zip(otp_key.iter()).for_each(|(s, k)| *s ^= *k);
        }

        tagged_bytes
    }

    /// Encrypt the seed and return an untagged byte array. Use this method
    /// when an encrypted seed is needed as a write key in [encrypted_write].
    fn encrypt_untagged(&self, otp_key: &[u8; 72]) -> [u8; 32] {
        let mut bytes = [0u8; 32];

        let skip_encrypting = match self {
            XorSeed::Twelve(seed) => {
                bytes[..16].copy_from_slice(seed);
                constant_time_eq_n(seed, &[0; 16])
            }
            XorSeed::TwentyFour(seed) => {
                bytes.copy_from_slice(seed);
                constant_time_eq_n(seed, &[0; 32])
            }
        };

        if !skip_encrypting {
            for (s, k) in bytes.iter_mut().zip(otp_key.iter()) {
                *s ^= *k;
            }
        }

        bytes
    }

    /// Undo the XOR operation to get the original [Seed].
    fn pin_hash_xor(self, xor_hash: &XorPinHash) -> Seed {
        match self {
            XorSeed::Twelve(mut seed) => {
                seed.iter_mut().zip(xor_hash.0).for_each(|(x, y)| *x ^= y);
                Seed::Twelve(seed)
            }
            XorSeed::TwentyFour(mut seed) => {
                seed.iter_mut().zip(xor_hash.0).for_each(|(x, y)| *x ^= y);
                Seed::TwentyFour(seed)
            }
        }
    }
}

#[derive(Clone)]
pub enum LoginAttempt {
    Success { auth_hash: AuthPinHash },
    Failure { attempts_left: u32, reason: LoginFailureReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFailureReason {
    IncorrectPin,
}

#[derive(Clone, ZeroizeOnDrop, Zeroize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct AesEntropy(pub [u8; 72]);

impl fmt::Debug for AesEntropy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AesEntropy").field(&"<redacted>").finish()
    }
}

impl Default for AesEntropy {
    fn default() -> Self { AesEntropy([0u8; 72]) }
}

/// SE block size in bytes.
const BLOCK_SIZE: usize = 32;

impl crate::Server {
    #[cfg(feature = "production")]
    /// Check config provisioning of the chip.
    pub fn check_config(&self) -> Result<(), Error> {
        let mut config = [0; 128];
        self.se.read_config_zone(&mut config)?;
        if config[87] == 0x55 {
            // config is still unlocked
            log::error!(
                "SE config is still unlocked! It should have been provisioned and locked at the factory!"
            );
            return Err(Error::SeNotProvisioned);
        } else {
            log::info!("SE config already locked");
        }

        Ok(())
    }

    /// One-time config and lockdown of the chip.
    ///
    /// NOTE: This is not the actual setup that will take place for production samples, those will be set up
    /// through the provisioning tool and be locked afterwards.
    #[cfg(not(feature = "production"))]
    pub fn setup_config(&self, firmware_timestamp: &FirmwareTimestamp) -> Result<(), Error> {
        let mut config = [0; 128];
        self.se.read_config_zone(&mut config)?;

        // Setup steps:
        // - write config zone data
        // - lock that
        // - write pairing secret (test it works)
        // - pick RNG value for words secret (and forget it)
        // - set all PIN values to known value (zeros)
        // - set all money secrets to known value (zeros)
        // - lock the data zone
        if config[87] == 0x55 {
            // config is still unlocked

            use crate::config;
            log::info!("writing SE config");

            config[16..16 + config::SE_CONFIG_1.len()].copy_from_slice(&config::SE_CONFIG_1);
            config[90..90 + config::SE_CONFIG_2.len()].copy_from_slice(&config::SE_CONFIG_2);

            self.se.write_config_zone(&config)?;
            log::info!("SE config written, locking");
            self.se.lock_config_zone_crc(crc16(&config))?;
            log::info!("SE config locked");

            if config[86] == 0x55 {
                // Data is still unlocked.
                log::info!("writing SE data");

                let unlocked = (config[88] as u16) | ((config[89] as u16) << 8);

                self.write_slot_unlocked(
                    Slot::IoProtectionSecret,
                    &[0; Slot::IoProtectionSecret.size()],
                    unlocked,
                )?;

                // Write predefined key to facilitate testing.
                self.write_slot_unlocked(Slot::KeycardAuthenticity, &DEV_KEYCARD_SECRET, unlocked)?;
                self.lock_slot(Slot::KeycardAuthenticity, unlocked)?;

                self.write_slot_unlocked(Slot::PinStretch, &rand::random::<[u8; BLOCK_SIZE]>(), unlocked)?;
                self.lock_slot(Slot::PinStretch, unlocked)?;

                self.write_slot_unlocked(Slot::PinAttempt, &rand::random::<[u8; BLOCK_SIZE]>(), unlocked)?;
                self.lock_slot(Slot::PinAttempt, unlocked)?;

                self.write_slot_unlocked(Slot::PinHash, &[0; Slot::PinHash.size()], unlocked)?;

                let mut match_count = [0u8; Slot::MatchCount.size()];
                match_count[0..4].copy_from_slice(1024u32.to_le_bytes().as_ref());
                match_count[4..8].copy_from_slice(1024u32.to_le_bytes().as_ref());
                self.write_slot_unlocked(Slot::MatchCount, &match_count, unlocked)?;

                self.write_slot_unlocked(Slot::LastGood, &[0; Slot::LastGood.size()], unlocked)?;
                self.write_slot_unlocked(Slot::FirmwareTimestamp, &firmware_timestamp.0, unlocked)?;

                self.write_slot_unlocked(Slot::Seed, &[0; Slot::Seed.size()], unlocked)?;

                if unlocked & (1 << (Slot::SecurityCheckPrivateKey as u8)) != 0 {
                    log::info!("generating security check key");
                    let security_check_pubkey = self.generate_security_check_key()?;
                    log::debug!("generated security check key: {:?}", security_check_pubkey);
                } else {
                    log::info!("security check key already written");
                }

                if unlocked & (1 << (Slot::FidoPrivateKey as u8)) != 0 {
                    log::info!("writing fido key");
                    let private_key = security::DEV_FIDO_ATTESTATION_PRIVATE_KEY;
                    self.se.priv_write(Slot::FidoPrivateKey as u16, &private_key).unwrap();
                    self.lock_slot(Slot::FidoPrivateKey, unlocked).unwrap();
                } else {
                    log::info!("fido key already written");
                }

                self.write_slot_unlocked(
                    Slot::SeedFingerprint,
                    &[0; Slot::SeedFingerprint.size()],
                    unlocked,
                )?;

                let mut aes_entropy = [0; BLOCK_SIZE * 3];
                for x in aes_entropy.iter_mut() {
                    *x = rand::random();
                }
                self.write_slot_unlocked(Slot::AesEntropy, &aes_entropy, unlocked)?;
            } else {
                log::info!("SE data already locked");
            }
        } else {
            log::info!("SE config already locked");
        }

        Ok(())
    }

    pub fn on_boot(&mut self) -> Result<(), Error> {
        let mut config = [0; 128];
        self.se.read_config_zone(&mut config)?;

        if config[86] == 0x55 {
            log::info!("writing SE data");
            // data is still unlocked
            let unlocked = (config[88] as u16) | ((config[89] as u16) << 8);
            self.write_slot_unlocked(Slot::IoProtectionSecret, &self.io_protection_secret, unlocked)?;
            self.lock_slot(Slot::IoProtectionSecret, unlocked)?;
            log::info!("locking SE data");
            self.se.lock_data_zone()?;
            log::info!("SE data locked");
        } else {
            log::info!("SE data already locked");
        }

        // Check Slot 8 config
        if config[36] != 0xc4 || config[37] != 0x4a {
            log::info!("Activating compatibility mode: aes_keys_in_slot_12");
            self.compatibility.aes_keys_in_slot_12 = true;
        }

        log::info!("reading security check key");
        let security_check_pubkey = self.get_pubkey(Slot::SecurityCheckPrivateKey)?;
        log::debug!("read security check key: {security_check_pubkey:?}");

        use p256::elliptic_curve::sec1::FromEncodedPoint;
        let fido_pubkey = self.get_pubkey(Slot::FidoPrivateKey)?;
        let fido_point = p256::EncodedPoint::from_untagged_bytes(&fido_pubkey.into());
        let fido_pubkey = p256::PublicKey::from_encoded_point(&fido_point).expect("invalid pubkey");
        log::info!("read fido pubkey SEC1: {:02x?}", fido_pubkey.to_sec1_bytes());

        Ok(())
    }

    /// Sets [Slot::Seed] and [Slot::PinHash] back to zeros.
    /// Sets [Slot::AesEntropy] to a new random 72-byte value.
    /// Updates login counters until there are [MAX_LOGIN_ATTEMPTS] login attempts.
    pub fn reset_auth_data(&self, lockout_options: LockoutOptions) -> Result<(), Error> {
        self.pair_unlock()?;

        // Write keys are 32 bytes long and the other slots hold data that is no larger than 72 bytes.
        let (buf32, mut buf72) = ([0u8; 32], [0u8; 72]);

        // The seed is always erased during lockout
        self.write_slot(Slot::Seed, &buf72[..Slot::Seed.size()])?;

        if lockout_options.seed_fingerprint {
            self.encrypted_write(
                Slot::SeedFingerprint,
                Slot::Seed,
                &buf32,
                &buf72[..Slot::SeedFingerprint.size()],
            )?;
        }

        self.encrypted_write(Slot::PinHash, Slot::Seed, &buf32, &buf72[..Slot::PinHash.size()])?;
        self.reset_login_counters(buf32)?;

        if lockout_options.aes_keys {
            let aes_entropy_slot = if self.compatibility.aes_keys_in_slot_12 {
                Slot::KeycardAuthenticity
            } else {
                Slot::AesEntropy
            };
            rand::thread_rng().fill_bytes(&mut buf72);
            self.encrypted_write(aes_entropy_slot, Slot::Seed, &buf32, &buf72)?;
        }

        Ok(())
    }

    /// Get an (untagged) uncompressed public key from private key slot.
    pub fn get_pubkey(&self, slot: Slot) -> Result<[u8; 64], Error> {
        let slot = match slot {
            Slot::SecurityCheckPrivateKey | Slot::FidoPrivateKey => slot as u16,
            // Not using catch-all here because we want to be explicit about which slots are valid.
            Slot::None
            | Slot::IoProtectionSecret
            | Slot::PinStretch
            | Slot::PinAttempt
            | Slot::PinHash
            | Slot::MatchCount
            | Slot::LastGood
            | Slot::FirmwareTimestamp
            | Slot::Seed
            | Slot::AesEntropy
            | Slot::KeycardAuthenticity
            | Slot::SeedFingerprint => return Err(Error::InvalidSlot),
        };

        self.pair_unlock()?;
        self.se.get_pubkey(slot).map_err(Error::CryptoAuthLib)
    }

    /// Calculate an HMAC over a 32-byte message using the [Slot::KeycardAuthenticity] slot as key.
    pub fn keycard_authenticity_mac(&self, msg: [u8; 32]) -> Result<[u8; 32], Error> {
        if self.compatibility.aes_keys_in_slot_12 {
            log::warn!("Compatibility mode: sending fake keycard MAC");
            return self
                .crypto
                .hmac256(DEV_KEYCARD_SECRET.to_vec(), msg.to_vec())
                .map_err(Error::Crypto)
                .map(|v| v[..32].try_into().unwrap());
        }
        self.pair_unlock()?;
        self.hmac32(Slot::KeycardAuthenticity, msg)
    }

    fn write_slot_unlocked(&self, slot: Slot, data: &[u8], unlocked: u16) -> Result<(), Error> {
        if unlocked & (1 << (slot as u8)) == 0 {
            return Ok(());
        }
        log::info!("writing slot {:?}, len {}", slot, data.len());
        self.write_slot(slot, data)
    }

    fn write_slot(&self, slot: Slot, data: &[u8]) -> Result<(), Error> {
        let mut block: [u8; BLOCK_SIZE];
        for (i, c) in data.chunks(BLOCK_SIZE).enumerate() {
            let i = i as u16;
            let slot = slot as u16;
            self.se.write(
                0x80 | 2,
                (i << 8) | (slot << 3),
                if c.len() == BLOCK_SIZE {
                    c
                } else {
                    block = [0; BLOCK_SIZE];
                    block[..c.len()].copy_from_slice(c);
                    &block
                },
                None,
            )?;
        }
        Ok(())
    }

    fn lock_slot(&self, slot: Slot, unlocked: u16) -> Result<(), Error> {
        if unlocked & (1 << (slot as u8)) == 0 {
            return Ok(());
        }
        self.se.lock_data_slot(slot as u16)?;
        Ok(())
    }

    /// Load Tempkey with a nonce value that we both know, but
    /// is random and we both know is random! Tricky!
    fn pick_nonce(&self, num_in: [u8; 20]) -> Result<[u8; 32], Error> {
        // Nonce command returns the RNG result, but not contents of TempKey
        let rand_out = self.se.nonce_rand(&num_in)?;
        // Hash stuff appropriately to get same number as chip did.
        // TempKey on the chip will be set to the output of SHA256 over
        // a message composed of my challenge, the RNG and 3 bytes of constants:
        //
        // return sha256(rndout + num_in + b'\x16\0\0').digest()
        sha256_batch(&self.crypto, &[&rand_out, &num_in, &[0x16, 0x00, 0x00]]).map_err(Error::Crypto)
    }

    /// CAUTION: The result from this function could be modified by an
    /// active attacker on the bus because the one-byte response from the chip
    /// is easily replaced. This command is useful for us to authorize actions
    /// inside the 508a/608a, like use of a specific key, but not for us to
    /// authenticate the 508a/608a or its contents/state.
    fn checkmac(&self, slot: Slot, secret: [u8; 32]) -> Result<(), Error> {
        let mut od: [u8; 32] = rand::random();
        od[13..].fill(0);
        let num_in: [u8; 20] = rand::random();
        let tempkey = self.pick_nonce(num_in)?;
        // Hash nonce and lots of other bits together
        let result = sha256_batch(
            &self.crypto,
            &[
                &secret,
                &tempkey,
                &od[0..4],
                &[0u8; 8],
                &od[4..7],
                &[0xEE],
                &od[7..11],
                &[0x01, 0x23],
                &od[11..13],
            ],
        )
        .map_err(Error::Crypto)?;

        // Content doesn't matter, but nice and visible:
        let challenge = b"(C) 2020 Foundation Devices Inc.";
        self.se.checkmac(0x01, slot as u16, challenge, &result, &od)?;

        Ok(())
    }

    /// Check the chip produces a hash over various things the same way we would
    /// meaning that we both know the shared secret and the state of stuff in
    /// the 508a is what we expect.
    fn checkmac_hard(&self, slot: Slot, secret: [u8; 32]) -> Result<(), Error> {
        let digest = self.gendig_slot(slot, &secret)?;
        // NOTE: we use this sometimes when we know the value is wrong, like
        // checking for blank pin codes... so not a huge error/security issue
        // if wrong here.
        if !self.is_correct_tempkey(digest)? {
            Err(Error::SeIncorrectTempkey)
        } else {
            Ok(())
        }
    }

    fn gendig_slot(&self, slot: Slot, slot_contents: &[u8; 32]) -> Result<[u8; 32], Error> {
        let num_in: [u8; 20] = rand::random();
        let tempkey = self.pick_nonce(num_in)?;

        // Using Zone=2="Data" => "KeyID specifies a slot in the Data zone".
        self.se.gendig(0x2, slot as u16, None)?;

        // We now have to match the digesting (hashing) that has happened on
        // the chip. No feedback at this point if it's right tho.
        //
        //   msg = hkey + b'\x15\x02' + ustruct.pack("<H", slot_num)
        //   msg += b'\xee\x01\x23' + (b'\0'*25) + challenge
        //   assert len(msg) == 32+1+1+2+1+2+25+32
        let args = [OP_GENDIG, 2, slot as u8, 0, 0xEE, 0x01, 0x23];

        sha256_batch(&self.crypto, &[slot_contents, &args, &[0u8; 25], &tempkey]).map_err(Error::Crypto)
    }

    /// Construct a digest over one of the two counters. Track what we think
    /// the digest should be, and ask the chip to do the same. Verify we match
    /// using MAC command (done elsewhere).
    fn gendig_counter(&self, counter: Counter, expected_value: u32) -> Result<[u8; 32], Error> {
        let num_in: [u8; 20] = rand::random();
        let tempkey = self.pick_nonce(num_in)?;

        // Using Zone=4="Counter" => "KeyID specifies the monotonic counter ID".
        self.se.gendig(0x4, counter as u16, None)?;

        // we now have to match the digesting (hashing) that has happened on
        // the chip. No feedback at this point if it's right tho.
        //
        //   msg = hkey + b'\x15\x02' + ustruct.pack("<H", slot_num)
        //   msg += b'\xee\x01\x23' + (b'\0'*25) + challenge
        //   assert len(msg) == 32+1+1+2+1+2+25+32
        //
        let args = [OP_GENDIG, 0x4, counter as u8, 0, 0xEE, 0x01, 0x23, 0x0];
        let expected_value_bytes = expected_value.to_le_bytes();

        sha256_batch(
            &self.crypto,
            &[
                // HKEY
                &[0u8; 32],
                &args,
                &expected_value_bytes,
                &[0u8; 20],
                &tempkey,
            ],
        )
        .map_err(Error::Crypto)
    }

    /// Check that TempKey is holding what we think it does. Uses the MAC
    /// command over contents of Tempkey and our shared secret.
    fn is_correct_tempkey(&self, expected_tempkey: [u8; 32]) -> Result<bool, Error> {
        #[allow(clippy::identity_op)]
        let mode: u8 = (1 << 6)  // Include full serial number
                 | (0 << 2)  // TempKey.SourceFlag == 0 == 'rand'
                 | (0 << 1)  // First 32 bytes are the shared secret
                 | (1 << 0); // Second 32 bytes are tempkey

        let resp = self.se.mac(mode, Slot::IoProtectionSecret as u16, None)?;

        // Duplicate the hash process, and then compare.
        let fixed = [
            OP_MAC,
            mode,
            Slot::IoProtectionSecret as u8,
            0x0,
            // eight zeros
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            // three zeros
            0,
            0,
            0,
            0xEE,
        ];

        let actual = sha256_batch(
            &self.crypto,
            &[
                &self.io_protection_secret,
                &expected_tempkey,
                &fixed,
                &self.serial_number[4..8],
                &self.serial_number[..4],
            ],
        )
        .map_err(Error::Crypto)?;

        Ok(constant_time_eq::constant_time_eq_32(&actual, &resp))
    }

    pub fn get_aes_entropy(&self, auth_hash: &AuthPinHash) -> Result<AesEntropy, Error> {
        let slot =
            if self.compatibility.aes_keys_in_slot_12 { Slot::KeycardAuthenticity } else { Slot::AesEntropy };
        let mut buf = [0u8; 72];
        self.encrypted_read(slot, Slot::PinHash, auth_hash.0, &mut buf)?;
        Ok(AesEntropy(buf))
    }

    /// Do the PIN check.
    pub fn pin_login_attempt(&self, pin: &Pin, otp_key: &[u8; 72]) -> Result<LoginAttempt, Error> {
        let auth_hash = self.pin_hash_attempt(pin, otp_key)?;

        if !self.is_main_pin(auth_hash.0)? {
            // PIN code is just wrong.
            // - nothing to update, since the chip's done it already
            return Ok(match self.get_last_success() {
                Ok(LastSuccess { attempts_left, .. }) => {
                    LoginAttempt::Failure { attempts_left, reason: LoginFailureReason::IncorrectPin }
                }
                Err(_) => {
                    LoginAttempt::Failure { attempts_left: 0, reason: LoginFailureReason::IncorrectPin }
                }
            });
        }

        Ok(LoginAttempt::Success { auth_hash })
    }

    /// Fetch the [XorSeed] from SE and undo the XOR operation to get the original [Seed].
    ///
    /// NOTE: Keeping the seed in memory is discouraged which is why this function and [Self::get_xor_seed]
    /// allows fetching it if knowledge of the PIN has been proven.
    pub fn get_seed(
        &self,
        auth_hash: &AuthPinHash,
        xor_hash: &XorPinHash,
        otp_key: &[u8; 72],
    ) -> Result<Seed, Error> {
        self.get_xor_seed(auth_hash, otp_key).map(|xor_seed| xor_seed.pin_hash_xor(xor_hash))
    }

    /// Fetch the [XorSeed] from SE.
    ///
    /// NOTE: Keeping the seed in memory is discouraged which is why this function and [Self::get_seed] allows
    /// fetching it if knowledge of the PIN has been proven.
    pub fn get_xor_seed(&self, auth_hash: &AuthPinHash, otp_key: &[u8; 72]) -> Result<XorSeed, Error> {
        let mut buf = [0; Slot::Seed.size()];

        log::info!("reading seed from SE");
        self.encrypted_read(Slot::Seed, Slot::PinHash, auth_hash.0, &mut buf)?;

        let (seed, unused) = buf.split_at(33);
        let seed_bytes_tagged: [u8; 33] = seed.try_into().expect("incorrect slice length");
        let xor_seed = XorSeed::decrypt_tagged(seed_bytes_tagged, otp_key);

        let unused_bytes_zeroed = constant_time_eq(unused, &[0; Slot::Seed.size() - 33]);

        // Make sure that bytes 33-71 (which are not part of the seed) of the SE secret are all zero.
        if !unused_bytes_zeroed {
            log::warn!("SE seed slot bytes 33-71 should be all zeros, but they are not");
        }

        Ok(xor_seed)
    }

    /// Write a new seed (after XORing and encrypting) and seed fingerprint into the SE.
    ///
    /// Returns an unencrypted [XorSeed] that has been stored in the SE.
    pub fn change_seed(
        &self,
        new_seed: &Seed,
        xor_hash: &XorPinHash,
        otp_key: &[u8; 72],
    ) -> Result<XorSeed, Error> {
        let new_seed_xored = new_seed.clone().pin_hash_xor(xor_hash);

        log::info!("writing new seed into SE");
        let new_seed_enc_tagged = new_seed_xored.encrypt_tagged(otp_key);
        self.pair_unlock()?;
        self.write_slot(Slot::Seed, &new_seed_enc_tagged)?;

        let (_, new_seed_enc) = new_seed_enc_tagged.split_last().expect("array is not empty");

        log::info!("updating seed fingerprint");
        let seed_fingerprint = seed_fingerprint(&self.crypto, new_seed).map_err(Error::Crypto)?;
        self.encrypted_write(
            Slot::SeedFingerprint,
            Slot::Seed,
            &new_seed_enc.try_into().expect("incorrect slice length"),
            &seed_fingerprint,
        )?;

        Ok(new_seed_xored)
    }

    /// Generate anti-phishing words based on device-specific data.
    ///
    /// The words are derived from a hash of:
    /// - The IO protection secret (device-specific)
    /// - The PIN prefix (first 4 digits)
    /// - The SE serial number (unique per device)
    /// - The seed fingerprint (unique per wallet)
    pub fn anti_phishing_words(
        &self,
        pin_prefix: &[u8],
        serial_number: &[u8; 9],
        seed_fingerprint: &[u8; 32],
    ) -> Result<[u8; 32], Error> {
        const ANTI_PHISHING_SALT: &[u8] = b"AntiPhishing";

        // Hash all the device-specific and wallet-specific data together
        let digest = sha256_batch(
            &self.crypto,
            &[&self.io_protection_secret, ANTI_PHISHING_SALT, pin_prefix, serial_number, seed_fingerprint],
        )
        .map_err(Error::Crypto)?;

        // Mix in the stretch key for additional security
        self.pair_unlock()?;
        let digest = self.hmac32(Slot::PinStretch, digest)?;

        // Final hash to protect the value read over the bus
        sha256_batch(&self.crypto, &[&self.io_protection_secret, ANTI_PHISHING_SALT, &digest[..]])
            .map_err(Error::Crypto)
    }

    pub fn set_pin(&self, xor_seed: &XorSeed, pin: &Pin, otp_key: &[u8; 72]) -> Result<(), Error> {
        // Calculate new PIN hashed value: will be slow for main pin.
        let pin_hash = self.pin_hash_attempt(pin, otp_key)?;

        let seed_enc = xor_seed.encrypt_untagged(otp_key);
        self.encrypted_write(Slot::PinHash, Slot::Seed, &seed_enc, &pin_hash.0)?;

        // Main pin is changing; reset counter to zero (good login) and our cache.
        self.reset_login_counters(pin_hash.0)?;

        Ok(())
    }

    pub fn change_pin(
        &self,
        new_pin: &Pin,
        xor_seed: XorSeed,
        old_xor_pin_hash: &XorPinHash,
        new_xor_pin_hash: &XorPinHash,
        otp_key: &[u8; 72],
    ) -> Result<(), Error> {
        // Calculate new PIN hashed value: will be slow for main pin.
        let new_pin_hash = self.pin_hash_attempt(new_pin, otp_key)?;
        let seed_enc = xor_seed.encrypt_untagged(otp_key);

        self.encrypted_write(Slot::PinHash, Slot::Seed, &seed_enc, &new_pin_hash.0)?;
        // Update the seed in the SE to match the newly set PIN. The plain seed value did not change so we
        // don't need to change the seed fingerprint.
        let new_seed_enc_tagged =
            xor_seed.pin_hash_xor(old_xor_pin_hash).pin_hash_xor(new_xor_pin_hash).encrypt_tagged(otp_key);
        self.write_slot(Slot::Seed, &new_seed_enc_tagged)?;

        // Main pin is changing; reset counter to zero (good login) and our cache.
        self.reset_login_counters(new_pin_hash.0)?;

        Ok(())
    }

    pub fn pin_is_zero(&self) -> Result<bool, Error> {
        self.pair_unlock()?;
        Ok(self.checkmac_hard(Slot::PinHash, [0; 32]).is_ok())
    }

    pub fn get_firmware_timestamp(&self) -> Result<FirmwareTimestamp, Error> {
        self.pair_unlock()?;
        let mut padded = [0; 32];
        self.se.read_zone(2, Slot::FirmwareTimestamp as u16, 0, 0, &mut padded)?;
        Ok(FirmwareTimestamp(padded[..4].try_into().unwrap()))
    }

    pub fn change_firmware_timestamp(&self, firmware_timestamp: &FirmwareTimestamp) -> Result<(), Error> {
        self.write_slot(Slot::FirmwareTimestamp, &firmware_timestamp.0)
    }

    #[cfg(not(feature = "production"))]
    pub fn generate_security_check_key(&self) -> Result<[u8; 64], Error> {
        let pubkey = self.se.genkey(Slot::SecurityCheckPrivateKey as u16)?;
        self.se.lock_data_slot(Slot::SecurityCheckPrivateKey as u16)?;
        Ok(pubkey)
    }

    pub fn sign_with_security_check_key(&self, msg: &[u8; 32]) -> Result<[u8; 64], Error> {
        self.pair_unlock()?;
        self.se.sign(Slot::SecurityCheckPrivateKey as u16, msg).map_err(Into::into)
    }

    pub fn sign_with_fido_key(&self, msg: &[u8; 32]) -> Result<[u8; 64], Error> {
        self.pair_unlock()?;
        self.se.sign(Slot::FidoPrivateKey as u16, msg).map_err(Into::into)
    }

    pub fn get_seed_fingerprint(&self) -> Result<[u8; 32], Error> {
        let mut fingerprint = [0; 32];
        self.pair_unlock()?;
        self.se.read_zone(2, Slot::SeedFingerprint as u16, 0, 0, &mut fingerprint)?;
        Ok(fingerprint)
    }

    fn encrypted_write(
        &self,
        data_slot: Slot,
        write_kn: Slot,
        write_key: &[u8; 32],
        data: &[u8],
    ) -> Result<(), Error> {
        for (num_blk, blk) in data.chunks(32).enumerate() {
            let mut tmp = [0; 32];
            tmp[..blk.len()].copy_from_slice(blk);
            self.encrypted_write32(data_slot, num_blk, write_kn, write_key, tmp)?;
        }
        Ok(())
    }

    fn encrypted_write32(
        &self,
        data_slot: Slot,
        blk: usize,
        write_kn: Slot,
        write_key: &[u8; 32],
        data: [u8; 32],
    ) -> Result<(), Error> {
        self.pair_unlock()?;
        // Generate a hash over shared secret and rng.
        let digest = self.gendig_slot(write_kn, write_key)?;

        // encrypt the data to be written.
        let mut body = [0; 32];

        body.iter_mut().zip(data.iter()).zip(digest.iter()).for_each(|((b, d), k)| {
            *b = *d ^ *k;
        });

        // make auth-mac to go with
        //    SHA-256(TempKey, Opcode, Param1, Param2, SN<8>, SN<0:1>, <25 bytes of zeros>, PlainTextData)
        //    msg = (dig
        //        + ustruct.pack('<bbH', OP.Write, args['p1'], args['p2'])
        //        + b'\xee\x01\x23'
        //        + (b'\0'*25)
        //        + new_value)
        //    assert len(msg) == 32+1+1+2+1+2+25+32
        //
        let p1 = 0x80 | 2; // 32 bytes into a data slot
        let p2_lsb = ((data_slot as u16) << 3) as u8;
        let p2_msb = blk as u8;
        let args = [OP_WRITE, p1, p2_lsb, p2_msb, 0xEE, 0x01, 0x23];

        let mac = sha256_batch(&self.crypto, &[&digest, &args, &[0u8; 25], &data]).map_err(Error::Crypto)?;

        self.se.write(p1, ((p2_msb as u16) << 8) | p2_lsb as u16, &body, Some(&mac))?;

        Ok(())
    }

    fn encrypted_read(
        &self,
        data_slot: Slot,
        read_kn: Slot,
        read_key: [u8; 32],
        data: &mut [u8],
    ) -> Result<(), Error> {
        for (blk, chunk) in data.chunks_mut(32).enumerate() {
            let mut tmp = [0; 32];
            self.encrypted_read32(data_slot, blk as u8, read_kn, read_key, &mut tmp)?;
            chunk.copy_from_slice(&tmp[..chunk.len()]);
        }

        Ok(())
    }

    fn encrypted_read32(
        &self,
        data_slot: Slot,
        blk: u8,
        read_kn: Slot,
        read_key: [u8; 32],
        data: &mut [u8],
    ) -> Result<(), Error> {
        assert_eq!(data.len(), 32, "Data buffer must be 32 bytes long");

        self.pair_unlock()?;
        let digest = self.gendig_slot(read_kn, &read_key)?;

        // read nth 32-byte "block"
        self.se.read_zone(2, data_slot as u16, blk, 0, data)?;

        for (l, r) in data.iter_mut().zip(digest.iter()) {
            *l ^= *r;
        }

        Ok(())
    }

    /// Read state about previous attempt(s) from AE. Calculate number of failures,
    /// and how many attempts are left.
    /// We don't verify the counter values themselves, because the attempt logic is
    /// implemented in the SE chip itself, so the only thing even an active attacker
    /// can do is modify what's displayed on the UI.
    pub fn get_last_success(&self) -> Result<LastSuccess, Error> {
        let slot = Slot::LastGood;

        self.pair_unlock()?;

        // Read counter value of last-good login. Important that this be authenticated.
        // - using first 32-bits only, others will be zero
        let mut padded = [0; 32];
        self.se.read_zone(2, slot as u16, 0, 0, &mut padded)?;

        self.pair_unlock()?;
        let tempkey = self.gendig_slot(slot, &padded)?;

        if !self.is_correct_tempkey(tempkey)? {
            return Err(Error::SeIncorrectTempkey);
        }

        // Read two values from data slots
        let lastgood = self.read_slot_as_counter_insecure(Slot::LastGood)?;
        let mut match_count = self.read_slot_as_counter_insecure(Slot::MatchCount)?;

        // Read the monotonically-increasing counter
        let counter = self.get_counter_insecure(0)?;

        let num_fails = if lastgood > counter {
            // monkey business, but impossible, right?!
            99
        } else {
            counter - lastgood
        };

        // NOTE: 5LSB of match_count should be stored as zero.
        match_count &= !31;
        // Typical case: some number of attempts left before death.
        let attempts_left = match_count.saturating_sub(counter);

        Ok(LastSuccess { num_fails, attempts_left })
    }

    /// Read (typically a) counter value held in a dataslot.
    /// The resulting value is _not_ authenticated, so this should only be used
    /// for non-security-critical data.
    ///
    /// - using first 32-bits only, others will be zero/ignored
    /// - but need to read whole thing for the digest check
    fn read_slot_as_counter_insecure(&self, slot: Slot) -> Result<u32, Error> {
        self.pair_unlock()?;
        let mut padded = [0; 32];
        self.se.read_zone(2, slot as u16, 0, 0, &mut padded)?;
        Ok(u32::from_le_bytes(padded[..4].try_into().unwrap()))
    }

    fn is_main_pin(&self, digest: [u8; 32]) -> Result<bool, Error> {
        self.pair_unlock()?;
        Ok(self.checkmac_hard(Slot::PinHash, digest).is_ok())
    }

    /// Update the login counters until their values are such that there are [MAX_LOGIN_ATTEMPTS] login
    /// attempts left.
    pub fn reset_login_counters(&self, digest: [u8; 32]) -> Result<(), Error> {
        let attempt_cnt = self.get_counter(Counter::LoginAttempt)?;

        // The weird math here is because the match count slot in the SE ignores the least
        // significant 5 bits, so the match count must be a multiple of 32. When a good
        // login occurs, we need to update both the match count and the monotonic counter.
        //
        // For example, if the monotonic counter was 19 and the match count was 32, and the
        // user just provided the correct PIN, you would normally just bump the match count
        // to 33, but since that is not a multiple of 32, we have to bump it to 64. That
        // would then give 64-19 = 45 login attempts remaining though, so further down,
        // in self.add_counter(), we bump the monotonic counter in a loop until there are
        // MAX_LOGIN_ATTEMPTS left (match count - counter0 = MAX_LOGIN_ATTEMPTS_LEFT).
        let match_count = (attempt_cnt + MAX_LOGIN_ATTEMPTS + 32) & !31;

        // The SE won't let the counter go past the match count, so we have to update the
        // match count first.

        // Set the new "match count"
        let mut tmp = [0; 32];
        tmp[..4].copy_from_slice(&match_count.to_le_bytes());
        tmp[4..8].copy_from_slice(&match_count.to_le_bytes());
        self.encrypted_write(Slot::MatchCount, Slot::PinHash, &digest, &tmp)?;

        // Increment the monotonic (attempt) counter until the difference between the match count and the
        // monotonic counter is MAX_LOGIN_ATTEMPTS.
        let incr = (match_count - MAX_LOGIN_ATTEMPTS) - attempt_cnt;
        let new_attempt_cnt = self.add_counter(Counter::LoginAttempt, incr)?;

        // Update the "last good" counter
        tmp.zeroize();
        tmp[..4].copy_from_slice(&new_attempt_cnt.to_le_bytes());
        self.encrypted_write32(Slot::LastGood, 0, Slot::PinHash, &digest, tmp)?;

        // NOTE: Some or all of the above writes could be blocked (trashed) by an
        // active MitM attacker, but that would be pointless since these are authenticated
        // writes, which have a MAC. They can't change the written value, due to the MAC, so
        // all they can do is block the write, and not control it's value. Therefore, they will
        // just be reducing attempt. Also, rate limiting not affected by anything here.

        Ok(())
    }

    /// Add-to and return a one-way counter's value. Have to go up in
    /// single-unit steps, but we can loop.
    fn add_counter(&self, counter: Counter, incr: u32) -> Result<u32, Error> {
        let mut result = 0;
        for _ in 0..incr {
            result = self.se.counter_increment(counter as u16)?;
        }

        // IMPORTANT: Always verify the counter's value because otherwise
        // nothing prevents an active MitM changing the value that we think
        // we just read. They could also stop us from incrementing the counter.
        let digest = self.gendig_counter(counter, result)?;

        if !self.is_correct_tempkey(digest)? {
            return Err(Error::SeIncorrectTempkey);
        }

        Ok(result)
    }

    /// Read a one-way counter.
    pub fn get_counter(&self, counter: Counter) -> Result<u32, Error> {
        let result = self.se.counter_read(counter as u16)?;

        // IMPORTANT: Always verify the counter's value because otherwise
        // nothing prevents an active MitM changing the value that we think
        // we just read.
        let digest = self.gendig_counter(counter, result)?;

        if !self.is_correct_tempkey(digest)? {
            return Err(Error::SeIncorrectTempkey);
        }

        Ok(result)
    }

    /// Read a one-way counter.
    /// The resulting value is _not_ authenticated, so this should only be used
    /// for non-security-critical data.
    pub fn get_counter_insecure(&self, counter_number: u16) -> Result<u32, Error> {
        let result = self.se.counter_read(counter_number)?;
        Ok(result)
    }

    fn hmac32(&self, slot: Slot, msg: [u8; 32]) -> Result<[u8; 32], Error> {
        let mut digest = [0; 32];
        // Start SHA w/ HMAC setup
        unsafe {
            self.se.sha_base(
                4,
                slot as u16,
                core::ptr::null(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )?
        };
        let mut digest_len = digest.len() as u16;
        // Send the contents to be hashed. Place the result in the output buffer.
        unsafe {
            self.se.sha_base(
                (0b11 << 6) | 2,
                msg.len() as u16,
                msg.as_ptr(),
                digest.as_mut_ptr(),
                (&mut digest_len) as *mut u16,
            )?;
        }
        Ok(digest)
    }

    fn pair_unlock(&self) -> Result<(), Error> {
        const MAX_ATTEMPTS: usize = 3;
        for _ in 0..MAX_ATTEMPTS - 1 {
            if self.checkmac(Slot::IoProtectionSecret, self.io_protection_secret).is_ok() {
                return Ok(());
            }
        }
        self.checkmac(Slot::IoProtectionSecret, self.io_protection_secret)
    }

    pub fn pin_hash_attempt(&self, pin: &Pin, otp_key: &[u8; 72]) -> Result<AuthPinHash, Error> {
        let digest = self.mixin_local_secrets(b"PIN", &pin.0, otp_key)?;

        // Mix in the stretch key
        self.pair_unlock()?;
        let digest = self.hmac32(Slot::PinStretch, digest)?;

        // Mix in the attempt key, incrementing the counter
        self.pair_unlock()?;
        let digest = self.hmac32(Slot::PinAttempt, digest)?;

        // Final value was just read over bus w/o any protection, so mix it again
        self.mixin_local_secrets(b"PIN", &digest, otp_key).map(AuthPinHash)
    }

    fn mixin_local_secrets(&self, prefix: &[u8], msg: &[u8], otp_key: &[u8; 72]) -> Result<[u8; 32], Error> {
        sha256_batch(&self.crypto, &[&self.io_protection_secret, prefix, msg, &otp_key[..]])
            .map_err(Error::Crypto)
    }
}

#[cfg(not(feature = "production"))]
fn crc16(data: &[u8]) -> u16 {
    let poly = 0x8005u16;
    let mut crc = 0u16;
    for d in data {
        for i in 0..8 {
            let data_bit = (d >> i) & 1;
            let crc_bit = (crc >> 15) as u8;
            crc <<= 1;
            if data_bit != crc_bit {
                crc ^= poly;
            }
        }
    }
    crc
}

#[derive(Debug)]
pub enum Error {
    /// Existing pin is wrong (during change attempt).
    OldAuthFail,
    InvalidSlot,
    CryptoAuthLib(cryptoauthlib::Error),
    Crypto(crypto::error::CryptoError),
    SeIncorrectTempkey,
    #[cfg(feature = "production")]
    SeNotProvisioned,
}

impl From<cryptoauthlib::Error> for Error {
    fn from(err: cryptoauthlib::Error) -> Self { Error::CryptoAuthLib(err) }
}
