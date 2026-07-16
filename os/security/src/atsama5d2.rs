// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{num::NonZero, sync::LazyLock, time::Duration};

use atsama5d27::{
    secumod::{Protections, Secumod, HW_SECUMOD_BASE},
    securam::HW_SECURAM_BASE,
    sfr::{Sfr, HW_SFR_BASE},
};
use constant_time_eq::constant_time_eq;
use crypto::error::CryptoError;
use rand::{Rng, RngCore};
use securam_manager::SecuramManager;
#[cfg(not(feature = "production"))]
use security::FirmwareTimestamp;
use security::{
    messages::*, AccessDenied, BluetoothChallengeSecret, DeviceId, GetDeviceIdError, LockoutOptions,
    LoginFailed, MasterKeyState, OsVersionInfo, Pin, ScChallengeError, ScError, ScProof, SecurityWord, Seed,
};
use xous::{keyos, DropDeallocate, MemoryFlags};

use crate::{
    config::{Counter, Slot},
    se_port::{self, AuthPinHash, LoginAttempt, SeedExtras, XorPinHash},
    CryptoApi,
};
use crate::{seed_fingerprint, sha256, sha256_batch};

dma::use_api!();
gpio::use_api!();
power_manager::use_api!();

pub fn new_pin(crypto: &CryptoApi, raw_pin: &str) -> Result<Pin, CryptoError> {
    sha256(crypto, raw_pin.as_bytes()).map(Pin)
}

const INTERRUPT_PROTECTIONS: Protections = Protections::from_bits_truncate(
    Protections::DBLFM.bits()
        | Protections::SHLDM.bits()
        | Protections::TPML.bits()
        | Protections::TPMH.bits()
        | Protections::VDDBUL.bits()
        | Protections::VDDBUH.bits()
        | Protections::JTAG.bits()
        | Protections::DET5.bits()
        | Protections::DET7.bits(),
);

const AES_DISK_KEY_ENCRYPTION_SALT: &[u8] = b"AES";

#[derive(server::Server)]
#[name = "os/security"]
pub struct Server {
    pub se: cryptoauthlib::Device,
    pub crypto: CryptoApi,
    pwr: PowerManagerApi,

    pub io_protection_secret: [u8; 32],
    pub serial_number: [u8; 9],

    /// Consists of:
    ///
    ///   1) SHA256 PIN hash used for authentication.
    ///   2) SHA512 hash of a PIN and an encryption salt. Used to perform XOR operations with the seed.
    pin_hash: Option<(AuthPinHash, XorPinHash)>,

    bluetooth_device_id: Option<[u8; 8]>,

    /// `SFR` is used to get the CPU's serial number.
    mcu_serial: u64,

    /// [DeviceId] cache.
    device_id: Option<DeviceId>,

    pub compatibility: Compatibility,

    pending_app_seed_request: Vec<server::ArchiveResponse<Result<[u8; 32], AccessDenied>>>,

    #[cfg(not(feature = "recovery-os"))]
    disk_encryption_keys_ready: bool,
    #[cfg(not(feature = "recovery-os"))]
    disk_encryption_keys_ready_subscribers:
        Vec<server::ScalarEventSubscriber<security::messages::DiskEncryptionKeysReady>>,
}

#[cfg(keyos)]
#[derive(Debug, server::Message)]
pub struct TamperEvent;

#[derive(Default, Clone)]
struct InterruptConnection;

impl server::CheckedPermissions for InterruptConnection {
    const NAME: &str = "os/security";
}

impl server::MessageAllowed<TamperEvent> for InterruptConnection {}

struct InterruptContext {
    conn: server::CheckedConn<InterruptConnection>,
    secumod: Secumod,
}

// Compatibility flags for older SE configs
#[derive(Debug, Default)]
pub struct Compatibility {
    // On an older config version, the AES keys were stored in slot 12 (now the keycard key slot),
    // and slot 8 was empty. On this config, the keycard key is unavailable, and the aes keys need
    // to be read from that slot.
    pub aes_keys_in_slot_12: bool,
}

impl server::BlockingArchiveHandler<SetSeedAndPin> for Server {
    fn handle(
        &mut self,
        msg: SetSeedAndPin,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetSeedAndPin as server::BlockingArchive>::Response {
        // Validate PIN length before processing
        crate::validate_raw_pin(&msg.pin.0)?;

        let pin = new_pin(&self.crypto, &msg.pin.0).map_err(|_| crate::PinError::AccessDenied)?;
        let xor_hash =
            XorPinHash::new(&self.crypto, &msg.pin.0).map_err(|_| crate::PinError::AccessDenied)?;

        let (xor_seed, otp_key) =
            self.change_seed_and_otp_key(&msg.seed, &xor_hash).map_err(|_| AccessDenied)?;

        let auth_hash = self.pin_hash_attempt(&pin, &otp_key).map_err(|_| crate::PinError::AccessDenied)?;
        self.set_pin(&xor_seed, &pin, &otp_key).map_err(|_| AccessDenied)?;
        self.setup_aes_keys(&auth_hash, &xor_hash).map_err(|_| AccessDenied)?;
        self.pin_hash = Some((auth_hash, xor_hash));

        Self::with_securam(|securam| securam.set_pin_entry_mode(msg.pin_entry.into()))
            .map_err(|_| crate::PinError::AccessDenied)?;

        Ok(())
    }
}

impl server::BlockingArchiveHandler<ChangePin> for Server {
    fn handle(
        &mut self,
        mut msg: ChangePin,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <ChangePin as server::BlockingArchive>::Response {
        // Validate PIN length before processing
        crate::validate_raw_pin(&msg.pin.0)?;

        let Some((old_auth_hash, old_xor_hash)) = self.pin_hash.as_ref() else {
            return Err(crate::PinError::AccessDenied);
        };

        let otp_key = read_otp_key();

        let xor_seed = msg
            .seed
            .take()
            .map(|seed| Ok(seed.pin_hash_xor(old_xor_hash)))
            .unwrap_or_else(|| self.get_xor_seed(old_auth_hash, &otp_key).map_err(|_| AccessDenied))?;

        let new_pin = new_pin(&self.crypto, &msg.pin.0).map_err(|_| crate::PinError::AccessDenied)?;
        let new_auth_hash =
            self.pin_hash_attempt(&new_pin, &otp_key).map_err(|_| crate::PinError::AccessDenied)?;
        let new_xor_hash =
            XorPinHash::new(&self.crypto, &msg.pin.0).map_err(|_| crate::PinError::AccessDenied)?;

        self.change_pin(&new_pin, xor_seed, old_xor_hash, &new_xor_hash, &otp_key)
            .map_err(|_| crate::PinError::AccessDenied)?;
        self.pin_hash = Some((new_auth_hash, new_xor_hash));

        Self::with_securam(|securam| securam.set_pin_entry_mode(msg.pin_entry.into()))
            .map_err(|_| crate::PinError::AccessDenied)?;

        Ok(())
    }
}

impl server::BlockingArchiveAsyncHandler<Login> for Server {
    fn handle(&mut self, request: server::ArchiveRequest<Login>, _context: &mut server::ServerContext<Self>) {
        let Ok(pin) = new_pin(&self.crypto, &request.message.pin.0)
            .inspect_err(|e| log::error!("Could not create PIN {e:?}"))
        else {
            return;
        };
        let Ok(xor_hash) = XorPinHash::new(&self.crypto, &request.message.pin.0)
            .inspect_err(|e| log::error!("Could not xor PIN {e:?}"))
        else {
            return;
        };

        random_login_delay();
        let Ok(login_attempt) = self
            .pin_login_attempt(&pin, &read_otp_key())
            .inspect_err(|e| log::error!("Could not xor PIN {e:?}"))
        else {
            return;
        };

        match login_attempt {
            LoginAttempt::Success { auth_hash } => {
                self.pin_hash = Some((auth_hash.clone(), xor_hash.clone()));
                if let Err(e) = self.setup_aes_keys(&auth_hash, &xor_hash) {
                    log::error!("Error setting AES keys: {e:?}");
                    return;
                }
                // Resetting the login counters takes a bit of time and can be done in the background,
                // let the client continue processing.
                request.response.respond(Ok(())).ok();

                if let Err(e) = self.reset_login_counters(auth_hash.0) {
                    log::error!("Error resetting login counters: {e:?}");
                }

                for pending in std::mem::take(&mut self.pending_app_seed_request) {
                    let res = self.get_app_seed(pending.pid(), &auth_hash, &xor_hash);
                    pending.respond(res).ok();
                }
            }
            LoginAttempt::Failure { attempts_left, reason, .. } => {
                log::warn!("Login failed: {:?}", reason);

                if attempts_left == 0 {
                    log::error!("All PIN attempts exhausted, erasing seed");
                    self.lockout(LockoutOptions::erase_seed_only()).expect("Could not lock out device");
                    self.pwr.reboot();
                }
                request.response.respond(Err(LoginFailed { attempts_left })).ok();
            }
        }
    }

    fn default_response() -> Result<(), LoginFailed> { Err(LoginFailed { attempts_left: 0 }) }
}

impl server::BlockingArchiveHandler<GetAttemptsRemaining> for Server {
    fn handle(
        &mut self,
        _msg: GetAttemptsRemaining,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetAttemptsRemaining as server::BlockingArchive>::Response {
        let last_success = self.get_last_success().map_err(|_| AccessDenied)?;
        Ok(last_success.attempts_left)
    }
}

impl server::BlockingArchiveHandler<GetFactoryResetCounter> for Server {
    fn handle(
        &mut self,
        _msg: GetFactoryResetCounter,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetFactoryResetCounter as server::BlockingArchive>::Response {
        self.get_counter_insecure(1).map_err(|_| AccessDenied)
    }
}

impl server::BlockingArchiveHandler<GetSeed> for Server {
    fn handle(
        &mut self,
        _msg: GetSeed,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetSeed as server::BlockingArchive>::Response {
        let Some((auth_hash, xor_hash)) = self.pin_hash.as_ref() else {
            return Err(AccessDenied);
        };

        let otp_key = read_otp_key();

        let otp_key_is_zeroed = constant_time_eq(&otp_key, &[0; 72]);
        if otp_key_is_zeroed {
            return Ok(None);
        }

        let master_seed = self.get_seed(auth_hash, xor_hash, &otp_key).map_err(|_| AccessDenied)?;
        Ok(Some(master_seed))
    }
}

impl server::BlockingArchiveHandler<SetSeed> for Server {
    fn handle(
        &mut self,
        mut msg: SetSeed,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetSeed as server::BlockingArchive>::Response {
        let Some((_, xor_hash)) = self.pin_hash.as_ref() else {
            return Err(AccessDenied);
        };
        let seed = std::mem::take(&mut msg.0);
        self.change_seed_and_otp_key(&seed, xor_hash).map_err(|_| AccessDenied)?;

        Ok(())
    }
}

impl server::BlockingArchiveAsyncHandler<GetAppSeed> for Server {
    fn handle(
        &mut self,
        request: server::ArchiveRequest<GetAppSeed>,
        _context: &mut server::ServerContext<Self>,
    ) {
        match self.pin_hash.as_ref() {
            Some((auth_hash, xor_hash)) => {
                let sender = request.response.pid();
                request.response.respond(self.get_app_seed(sender, auth_hash, xor_hash)).ok();
            }
            None => self.pending_app_seed_request.push(request.response),
        }
    }

    fn default_response() -> <GetAppSeed as server::BlockingArchive>::Response { Err(AccessDenied) }
}

impl server::BlockingArchiveHandler<GetFirmwareTimestamp> for Server {
    fn handle(
        &mut self,
        _msg: GetFirmwareTimestamp,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetFirmwareTimestamp as server::BlockingArchive>::Response {
        self.get_firmware_timestamp().map_err(|_| AccessDenied)
    }
}

impl server::BlockingArchiveHandler<SetFirmwareTimestamp> for Server {
    fn handle(
        &mut self,
        msg: SetFirmwareTimestamp,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetFirmwareTimestamp as server::BlockingArchive>::Response {
        self.change_firmware_timestamp(&msg.0).map_err(|_| AccessDenied)
    }
}

impl server::BlockingScalarHandler<Logout> for Server {
    fn handle(
        &mut self,
        _msg: Logout,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Logout as server::BlockingScalar>::Response {
        self.pin_hash = None;
    }
}

impl server::BlockingScalarHandler<LoggedIn> for Server {
    fn handle(
        &mut self,
        _msg: LoggedIn,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <LoggedIn as server::BlockingScalar>::Response {
        self.pin_hash.is_some()
    }
}

impl server::BlockingArchiveHandler<Lockout> for Server {
    fn handle(
        &mut self,
        msg: Lockout,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Lockout as server::BlockingArchive>::Response {
        self.lockout(msg.lockout_options)?;
        if msg.reboot {
            self.pwr.reboot();
        }
        Ok(())
    }
}

impl server::BlockingArchiveHandler<SignWithSecurityCheckKey> for Server {
    fn handle(
        &mut self,
        msg: SignWithSecurityCheckKey,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SignWithSecurityCheckKey as server::BlockingArchive>::Response {
        self.sign_with_security_check_key(&msg.0).map_err(|_| AccessDenied)
    }
}

impl server::BlockingArchiveHandler<SignWithFidoKey> for Server {
    fn handle(
        &mut self,
        msg: SignWithFidoKey,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SignWithFidoKey as server::BlockingArchive>::Response {
        self.sign_with_fido_key(&msg.0).map_err(|_| AccessDenied)
    }
}

impl server::BlockingArchiveHandler<GetFidoPubkey> for Server {
    fn handle(
        &mut self,
        _msg: GetFidoPubkey,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetFidoPubkey as server::BlockingArchive>::Response {
        self.get_pubkey(Slot::FidoPrivateKey).map_err(|_| AccessDenied)
    }
}

impl server::BlockingArchiveHandler<GetSecurityWords> for Server {
    fn handle(
        &mut self,
        msg: GetSecurityWords,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetSecurityWords as server::BlockingArchive>::Response {
        let seed_fingerprint = self.get_seed_fingerprint().map_err(|_| AccessDenied)?;
        let words = self
            .anti_phishing_words(&msg.pin_prefix, &self.serial_number, &seed_fingerprint)
            .map_err(|_| AccessDenied)?;
        let mnemonic = bip39::Mnemonic::from_entropy(&words).expect("from_entropy");
        let mut words = mnemonic.word_indices();
        Ok([SecurityWord(words.next().unwrap()), SecurityWord(words.next().unwrap())])
    }
}

impl server::BlockingArchiveHandler<GetSeedFingerprint> for Server {
    fn handle(
        &mut self,
        _msg: GetSeedFingerprint,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetSeedFingerprint as server::BlockingArchive>::Response {
        self.get_seed_fingerprint().map_err(|_| AccessDenied)
    }
}

impl server::BlockingArchiveHandler<ComputeSeedFingerprint> for Server {
    fn handle(
        &mut self,
        msg: ComputeSeedFingerprint,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <ComputeSeedFingerprint as server::BlockingArchive>::Response {
        seed_fingerprint(&self.crypto, &msg.0).map_err(|_| AccessDenied)
    }
}

impl server::BlockingArchiveHandler<GetOsVersionInfo> for Server {
    fn handle(
        &mut self,
        _msg: GetOsVersionInfo,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetOsVersionInfo as server::BlockingArchive>::Response {
        let Some(securam_manager::OsArguments::NormalMode { keyos_version, bootloader_version, .. }) =
            Server::with_securam(|securam| securam.os_arguments().cloned().ok())
        else {
            return Ok(None);
        };

        Ok(Some(OsVersionInfo { keyos_version, bootloader_version }))
    }
}

impl server::BlockingArchiveHandler<GetBootloaderBuildDate> for Server {
    fn handle(
        &mut self,
        _msg: GetBootloaderBuildDate,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetBootloaderBuildDate as server::BlockingArchive>::Response {
        #[cfg(feature = "recovery-os")]
        {
            let Some(securam_manager::OsArguments::RecoveryMode { bootloader_build_date, .. }) =
                Server::with_securam(|securam| securam.os_arguments().cloned().ok())
            else {
                return Ok(None);
            };

            Ok(Some(bootloader_build_date))
        }

        #[cfg(not(feature = "recovery-os"))]
        Ok(None)
    }
}

impl server::BlockingArchiveHandler<ScChallenge> for Server {
    fn handle(
        &mut self,
        msg: ScChallenge,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <ScChallenge as server::BlockingArchive>::Response {
        use p256::{
            ecdsa::{self, signature::hazmat::PrehashVerifier},
            elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint},
        };

        static SC_PUBKEY: LazyLock<ecdsa::VerifyingKey> = LazyLock::new(|| {
            hex::decode("0356C31C74AD5CFA2481C12CB2BDD86EE2B1AF423FD1720D6F7FE1D55448B66183")
                .map(|bytes| p256::EncodedPoint::from_bytes(&bytes).expect("invalid pubkey bytes"))
                .map(|point| ecdsa::VerifyingKey::from_encoded_point(&point).expect("invalid pubkey"))
                .expect("invalid hex string")
        });

        log::debug!("handling challenge {msg:?}");

        let deadline_expired = {
            let deadline = u64::from_le_bytes(msg.0[32..40].try_into().expect("invalid slice size"));
            let timestamp_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("SystemTime::now() should be after UNIX_EPOCH")
                .as_secs();
            log::debug!("deadline comparison {deadline} {timestamp_secs}");
            timestamp_secs > deadline
        };
        if deadline_expired {
            return Err(ScChallengeError::Sc(ScError::DeadlineExpired));
        }

        let challenge = &msg.0[..40];
        let signature = &msg.0[40..];

        let Ok(signature) = ecdsa::Signature::from_slice(signature) else {
            return Err(ScChallengeError::Sc(ScError::InvalidSignature));
        };

        let digest = sha256(&self.crypto, challenge).map_err(ScChallengeError::Crypto)?;

        if SC_PUBKEY.verify_prehash(&digest, &signature).is_err() {
            return Err(ScChallengeError::Sc(ScError::InvalidSignature));
        };

        let mut proof = Vec::with_capacity(ScProof::SIZE);
        proof.extend_from_slice(challenge);
        let pubkey = self
            .get_pubkey(Slot::SecurityCheckPrivateKey)
            .map(|pubkey| {
                // Ser/de roundtrip to compress the pubkey to 33 bytes.
                let point = p256::EncodedPoint::from_untagged_bytes(&pubkey.into());
                let pubkey = p256::PublicKey::from_encoded_point(&point).expect("invalid pubkey");
                pubkey.to_encoded_point(true).to_bytes()
            })
            .map_err(ScChallengeError::from)?;
        proof.extend_from_slice(&pubkey);
        let nonce: [u8; 32] = rand::random();
        proof.extend_from_slice(&nonce);
        let bootloader_version = {
            // Pad with zeros to fit the proof format.
            let mut buf = [0u8; 20];
            let os_args = Server::with_securam(|securam| securam.os_arguments().cloned())
                .map_err(|err| ScChallengeError::Internal(err.to_string()))?;
            let securam_manager::OsArguments::NormalMode { mut bootloader_version, .. } = os_args else {
                return Err(ScChallengeError::AccessDenied);
            };

            // replace space with 0 for sc-server
            for byte in &mut bootloader_version {
                if *byte == b' ' {
                    *byte = 0;
                }
            }

            buf[..bootloader_version.len()].copy_from_slice(&bootloader_version);
            buf
        };
        proof.extend_from_slice(&bootloader_version);

        assert_eq!(
            proof.len(),
            ScProof::SIZE - 64,
            "ScChallenge proof size mismatch: expected {}, got {}",
            ScProof::SIZE - 64,
            proof.len()
        );

        let digest = sha256(&self.crypto, &proof).map_err(ScChallengeError::Crypto)?;

        let signature = self.sign_with_security_check_key(&digest).map_err(ScChallengeError::from)?;
        proof.extend_from_slice(&signature);

        // This thing is a hash of the extra entropy and some string. See
        // boot/keyos-boot/src/securam.rs
        let security_check_secret = Self::with_securam(|securam| securam.security_check_secret().cloned())
            .map_err(|e| ScChallengeError::Internal(e.to_string()))?;

        // We XOR the first 32 bytes of proof with the first 32 bytes of the secret and then do the same thing
        // on the sc-server (online)
        for (proof_byte, key_byte) in proof[..32].iter_mut().zip(security_check_secret.iter()) {
            *proof_byte ^= *key_byte;
        }

        let proof: [u8; ScProof::SIZE] = proof.try_into().expect("invalid vec size");
        Ok(ScProof(proof))
    }
}

impl server::BlockingArchiveHandler<IsPinSet> for Server {
    fn handle(
        &mut self,
        _msg: IsPinSet,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <IsPinSet as server::BlockingArchive>::Response {
        self.pin_is_zero().map_err(|_| AccessDenied).map(|is_zero| !is_zero)
    }
}

impl From<se_port::Error> for ScChallengeError {
    fn from(err: se_port::Error) -> Self {
        match err {
            se_port::Error::OldAuthFail => ScChallengeError::AccessDenied,
            se_port::Error::CryptoAuthLib(e) => ScChallengeError::CryptoAuthLib(e.status),
            se_port::Error::Crypto(e) => ScChallengeError::Crypto(e),
            se_port::Error::InvalidSlot => ScChallengeError::Internal(String::from("invalid slot")),
            se_port::Error::SeIncorrectTempkey => {
                ScChallengeError::Internal(String::from("incorrect SE tempkey"))
            }
            #[cfg(feature = "production")]
            se_port::Error::SeNotProvisioned => {
                ScChallengeError::Internal(String::from("SE not provisioned"))
            }
        }
    }
}

impl server::BlockingArchiveHandler<GetDeviceId> for Server {
    fn handle(
        &mut self,
        _msg: GetDeviceId,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetDeviceId as server::BlockingArchive>::Response {
        if let Some(device_id) = self.device_id {
            return Ok(device_id);
        }

        let mut se_serial_number = [0u8; 9];
        self.se
            .read_serial_number(&mut se_serial_number)
            .map_err(|err| GetDeviceIdError::CryptoAuthLib(err.status))?;
        let mcu_serial_number_bytes = self.mcu_serial.to_le_bytes();
        let bt_dev_id = self.bluetooth_device_id.ok_or(GetDeviceIdError::NoBluetoothSerialYet)?;

        // Perform a double SHA-256 hash to generate the device ID.
        let input = [&se_serial_number[..], &mcu_serial_number_bytes[..], &bt_dev_id[..]];
        let sha_first = sha256_batch(&self.crypto, &input).map_err(GetDeviceIdError::Crypto)?;
        let device_id = sha256(&self.crypto, &sha_first).map(DeviceId).map_err(GetDeviceIdError::Crypto)?;
        self.device_id = Some(device_id);

        Ok(device_id)
    }
}

impl server::BlockingArchiveHandler<KeycardAuthenticityMac> for Server {
    fn handle(
        &mut self,
        msg: KeycardAuthenticityMac,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <KeycardAuthenticityMac as server::BlockingArchive>::Response {
        self.keycard_authenticity_mac(msg.0).map_err(|_| AccessDenied)
    }
}

impl server::BlockingArchiveHandler<GetBluetoothChallengeSecret> for Server {
    fn handle(
        &mut self,
        _msg: GetBluetoothChallengeSecret,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> BluetoothChallengeSecret {
        Self::with_securam(|securam| BluetoothChallengeSecret {
            secret: *securam.bluetooth_challenge_secret().expect("SECURAM corrupt"),
            sent: securam.bluetooth_challenge_secret_sent().expect("SECURAM corrupt"),
        })
    }
}

impl server::BlockingScalarHandler<SetBluetoothCheckSecretSent> for Server {
    fn handle(
        &mut self,
        _msg: SetBluetoothCheckSecretSent,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        Self::with_securam(|securam| securam.set_bluetooth_challenge_secret_sent().expect("SECURAM corrupt"))
    }
}

impl server::BlockingArchiveHandler<SetBluetoothDeviceId> for Server {
    fn handle(
        &mut self,
        msg: SetBluetoothDeviceId,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.bluetooth_device_id = Some(msg.0);
    }
}

impl server::BlockingArchiveHandler<GetRandom> for Server {
    fn handle(
        &mut self,
        _msg: GetRandom,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetRandom as server::BlockingArchive>::Response {
        let num_in = [0u8; 32];
        let mut rand_out = self.se.nonce_rand(&num_in).map_err(|_| AccessDenied)?;

        for (out, rand_byte) in rand_out.iter_mut().zip(rand::random::<[u8; 32]>()) {
            *out ^= rand_byte;
        }

        Ok(rand_out)
    }
}

impl server::BlockingArchiveHandler<GetPinEntryMode> for Server {
    fn handle(
        &mut self,
        _msg: GetPinEntryMode,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetPinEntryMode as server::BlockingArchive>::Response {
        Self::with_securam(|securam| securam.pin_entry_mode().unwrap_or(0).into())
    }
}

impl server::BlockingScalarHandler<GetMasterKeyState> for Server {
    fn handle(
        &mut self,
        _msg: GetMasterKeyState,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetMasterKeyState as server::BlockingScalar>::Response {
        let is_fingerprint_set = self
            .get_seed_fingerprint()
            .map(|fingerprint| fingerprint.iter().any(|&b| b != 0))
            .unwrap_or(false);
        let is_otp_set = read_otp_key().iter().any(|&b| b != 0);

        if !is_fingerprint_set {
            MasterKeyState::Onboarding
        } else {
            if is_otp_set {
                MasterKeyState::Normal
            } else {
                MasterKeyState::Erased
            }
        }
    }
}

impl server::ScalarHandler<TamperEvent> for Server {
    fn handle(&mut self, _msg: TamperEvent, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        log::error!("Tamper detected, erasing sensitive SE data");
        self.lockout(LockoutOptions::erase_seed_only()).expect("Could not lock out");
        self.pwr.reboot();
        panic!("Tamper happened but did not reboot");
    }
}

#[cfg(not(feature = "recovery-os"))]
impl server::ScalarEventSubscriptionHandler<security::messages::SubscribeDiskEncryptionKeysReady> for Server {
    fn handle(
        &mut self,
        _msg: security::messages::SubscribeDiskEncryptionKeysReady,
        subscriber: server::ScalarEventSubscriber<security::messages::DiskEncryptionKeysReady>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        if self.disk_encryption_keys_ready {
            if let Err(e) = subscriber.send(&security::messages::DiskEncryptionKeysReady) {
                log::warn!("DiskEncryptionKeysReady replay send failed: {e:?}");
            }
        } else {
            self.disk_encryption_keys_ready_subscribers.push(subscriber);
        }
        Ok(())
    }
}

impl server::Server for Server {
    fn on_start(&mut self, _context: &mut server::ServerContext<Self>) {
        self.setup_secumod();
        #[cfg(feature = "production")]
        self.check_config().unwrap();
        #[cfg(not(feature = "production"))]
        self.setup_config(&FirmwareTimestamp::default()).unwrap();
        if let Err(e) = self.on_boot() {
            #[cfg(feature = "recovery-os")]
            log::error!("Unable to communicate with SE: {e:?}");
            #[cfg(not(feature = "recovery-os"))]
            panic!("Unable to communicate with SE: {e:?}");
        }
    }
}

impl Default for Server {
    fn default() -> Self {
        // Wait for gpio server to mux FLEXCOM2 pins.
        let _gpio = GpioApi::default();

        let pwr = PowerManagerApi::default();
        pwr.enable_peripheral(atsama5d27::pmc::PeripheralId::Flexcom2).unwrap();

        let sfr_mem = xous::map_memory(
            Some(NonZero::new(HW_SFR_BASE).unwrap()),
            None,
            keyos::PAGE_SIZE,
            MemoryFlags::DEV,
        )
        .map(DropDeallocate::new)
        .expect("map SFR memory");

        let sfr_addr = sfr_mem.as_ptr() as u32;
        log::debug!("SFR mapped at 0x{:08x}", sfr_addr);
        let sfr = Sfr::with_alt_base_addr(sfr_addr);

        let mcu_serial = sfr.serial_number();
        log::debug!("Serial number: 0x{:08x}{:08x}", mcu_serial >> 32, mcu_serial & 0xFFFFFFFF);

        let se = cryptoauthlib::Device::init(dma_permissions::DmaPermissions).unwrap();
        let mut se_serial = [0; 9];
        se.read_serial_number(&mut se_serial).unwrap();
        Self {
            se,
            crypto: CryptoApi::default(),
            pwr,
            io_protection_secret: Self::with_securam(|securam| {
                *securam.io_protection_secret().expect("SECURAM corrupt")
            }),
            serial_number: se_serial,
            pin_hash: None,
            bluetooth_device_id: None,
            mcu_serial,
            device_id: None,
            compatibility: Default::default(),
            pending_app_seed_request: Default::default(),
            #[cfg(not(feature = "recovery-os"))]
            disk_encryption_keys_ready: false,
            #[cfg(not(feature = "recovery-os"))]
            disk_encryption_keys_ready_subscribers: Vec::new(),
        }
    }
}

impl Server {
    fn with_securam<R>(f: impl FnOnce(&mut SecuramManager) -> R) -> R {
        Self::try_with_securam(f).expect("SECURAM corrupt")
    }

    fn try_with_securam<R>(f: impl FnOnce(&mut SecuramManager) -> R) -> Result<R, securam_manager::Error> {
        log::debug!("Mapping SECURAM");

        let securam_mem = DropDeallocate::new(
            xous::map_memory(
                Some(NonZero::new(HW_SECURAM_BASE).unwrap()),
                None,
                keyos::PAGE_SIZE,
                MemoryFlags::W | MemoryFlags::DEV,
            )
            .expect("mapmemory"),
        );
        let securam_addr = securam_mem.as_ptr() as u32;
        log::debug!("SECURAM mapped at 0x{:08x}", securam_addr);
        let mut securam_manager = unsafe { SecuramManager::new(securam_mem.as_mut_ptr())? };
        Ok(f(&mut securam_manager))
    }

    fn setup_secumod(&self) {
        let secumod_mem = xous::map_memory(
            Some(NonZero::new(HW_SECUMOD_BASE).unwrap()),
            None,
            keyos::PAGE_SIZE,
            MemoryFlags::W | MemoryFlags::DEV,
        )
        .expect("map Secumod memory");

        let secumod_addr = secumod_mem.as_ptr() as u32;
        log::debug!("secumod mapped at 0x{:08x}", secumod_addr);

        let secumod = Secumod::with_alt_base_addr(secumod_addr);
        log::debug!("Secumod status: {:?}", secumod.system_status());

        secumod.with_protection_registers(|regs| {
            regs.enable_protections_interrupt(INTERRUPT_PROTECTIONS);
        });
        let int_ctx = Box::into_raw(Box::new(InterruptContext { conn: Default::default(), secumod }));
        xous::claim_interrupt(
            xous::arch::irq::IrqNumber::Secumod,
            secumod_irq_handler,
            int_ctx as *mut usize,
        )
        .expect("Could not claim Secumod interrupt");
    }

    fn lockout(&mut self, lockout_options: LockoutOptions) -> Result<(), AccessDenied> {
        self.reset_auth_data(lockout_options).map_err(|_| AccessDenied)?;

        // This might fail with "securam corrupt" if called from the
        // tamper event handler. In that case the securam is already
        // cleared, so we should be OK.
        Self::try_with_securam(|securam| {
            // SAFETY: SECURAM fields address is valid and properly aligned.
            unsafe {
                securam.clear();
            }
        })
        .ok();

        self.se.counter_increment(Counter::FactoryReset as u16).map_err(|_| AccessDenied)?;

        Ok(())
    }

    /// Write the new seed into SE and generate a new random OTP key to write into SECURAM.
    ///
    /// Returns the new [XORed seed](se_port::XorSeed) and OTP key.
    fn change_seed_and_otp_key(
        &self,
        seed: &Seed,
        xor_hash: &XorPinHash,
    ) -> Result<(se_port::XorSeed, [u8; 72]), se_port::Error> {
        let mut new_otp_key = [0u8; 72];
        rand::thread_rng().fill_bytes(&mut new_otp_key);
        Self::with_securam(|securam| securam.set_otp_key(&new_otp_key)).expect("SECURAM error");

        let xor_seed = self.change_seed(seed, xor_hash, &new_otp_key)?;

        Ok((xor_seed, new_otp_key))
    }

    fn setup_aes_keys(
        &mut self,
        auth_hash: &AuthPinHash,
        xor_hash: &XorPinHash,
    ) -> Result<(), se_port::Error> {
        Self::with_securam(|securam| -> Result<(), se_port::Error> {
            let (aes_key0, aes_key1) = securam.disk_encryption_keys().expect("SECURAM corrupt");
            let keys_are_set = !(aes_key0.is_zero() || aes_key1.is_zero());
            if keys_are_set {
                log::debug!("AES keys already set (and none of them are zero), skipping setup");
                return Ok(());
            }

            let seed = self.get_seed(auth_hash, xor_hash, &read_otp_key())?;
            let entropy = self.get_aes_entropy(auth_hash)?;

            let seed_digest = sha256_batch(&self.crypto, &[seed.bytes(), AES_DISK_KEY_ENCRYPTION_SALT])
                .map_err(se_port::Error::Crypto)?;

            let mut aes_key0: [u8; 32] = entropy.0[0..32]
                .iter()
                .zip(&seed_digest)
                .map(|(&x, &y)| x ^ y)
                .collect::<Vec<_>>()
                .try_into()
                .expect("incorrect vector length");
            let mut aes_key1: [u8; 32] = entropy.0[32..64]
                .iter()
                .zip(&seed_digest)
                .map(|(&x, &y)| x ^ y)
                .collect::<Vec<_>>()
                .try_into()
                .expect("incorrect vector length");

            // Volumes formatted before 1.3.0 were encrypted with these keys
            // word-swapped (set_key loaded each 4-byte word big-endian), so keep
            // deriving the swapped form or those volumes decrypt to garbage.
            for word in aes_key0.chunks_exact_mut(4).chain(aes_key1.chunks_exact_mut(4)) {
                word.reverse();
            }

            securam.set_disk_encryption_keys((&aes_key0, &aes_key1)).expect("SECURAM corrupt");
            Ok(())
        })?;

        // Keys can already be present after a security-server restart (SECURAM
        // persists), so signal unconditionally or subscribers wait forever.
        #[cfg(not(feature = "recovery-os"))]
        self.signal_disk_encryption_keys_ready();
        Ok(())
    }

    #[cfg(not(feature = "recovery-os"))]
    fn signal_disk_encryption_keys_ready(&mut self) {
        self.disk_encryption_keys_ready = true;
        for sub in self.disk_encryption_keys_ready_subscribers.drain(..) {
            if let Err(e) = sub.send(&security::messages::DiskEncryptionKeysReady) {
                log::warn!("DiskEncryptionKeysReady send failed: {e:?}");
            }
        }
    }

    fn get_app_seed(
        &self,
        sender: xous::PID,
        auth_hash: &AuthPinHash,
        xor_hash: &XorPinHash,
    ) -> Result<[u8; 32], AccessDenied> {
        let master_seed =
            self.get_seed(auth_hash, xor_hash, &read_otp_key()).map_err(|_| AccessDenied)?.to_vec();
        let sender_app_id =
            xous::get_app_id(sender).map_err(|_| AccessDenied)?.ok_or(AccessDenied)?.0.to_vec();

        let app_seed: [u8; 32] = self
            .crypto
            .hmac256(sender_app_id, master_seed)
            .map_err(|_| AccessDenied)?
            .try_into()
            .expect("incorrect slice length");

        Ok(app_seed)
    }
}

fn read_otp_key() -> [u8; 72] {
    Server::with_securam(|securam| securam.otp_key().cloned()).expect("SECURAM error")
}

/// Sleep for a random amount of time between 0 and 10 percent of the total login duration. Used to prevent
/// side-channel or fault injection attacks.
fn random_login_delay() {
    const MAX_LOGIN_DELAY_MS: u64 = 150;
    let rand_delay_ms = rand::thread_rng().gen_range(0..=MAX_LOGIN_DELAY_MS);
    std::thread::sleep(Duration::from_millis(rand_delay_ms));
}

fn secumod_irq_handler(_irq_no: usize, arg: *mut usize) {
    let context = unsafe { &*(arg as *const InterruptContext) };
    // Mask away interrupts to be able to get out of IRQ mode without acknowledging the tamper,
    // so the bootloader can detect it and display the message.
    context
        .secumod
        .with_protection_registers(|regs| regs.disable_protections_interrupt(INTERRUPT_PROTECTIONS));
    if let Err(e) = context.conn.send_scalar_nowait(TamperEvent) {
        // This should only happen if the CID is invalid for some reason.
        // We can't really panic!() here, but at least show an error.
        log::error!("could not send TamperEvent message: {e:?}");
    }
}
