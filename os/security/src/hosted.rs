// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt;

use security::{messages::*, AppSeed, PinEntryMode, Seed, MAX_LOGIN_ATTEMPTS};
use security::{
    AccessDenied, BluetoothChallengeSecret, DeviceId, FirmwareTimestamp, LoginFailed, MasterKeyState,
    OsVersionInfo, ScProof, SecurityWord,
};

use crate::{seed_fingerprint, validate_raw_pin, CryptoApi};

const DATA_FILE_NAME: &str = "hosted_security_data.json";

#[derive(server::Server)]
#[name = "os/security"]
pub struct Server {
    data: SecurityData,
    crypto: CryptoApi,
    logged_in: bool,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SecurityData {
    raw_pin: String,
    login_attempts: u32,
    factory_reset_counter: u32,
    seed: Vec<u8>,
    seed_fingerprint: Vec<u8>,
    firmware_timestamp: [u8; 4],
    pin_entry_mode: u8,
}

impl fmt::Debug for SecurityData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecurityData")
            .field("raw_pin", &"<redacted>")
            .field("login_attempts", &self.login_attempts)
            .field("factory_reset_counter", &self.factory_reset_counter)
            .field("seed", &"<redacted>")
            .field("seed_fingerprint", &self.seed_fingerprint)
            .field("firmware_timestamp", &self.firmware_timestamp)
            .field("pin_entry_mode", &self.pin_entry_mode)
            .finish()
    }
}

impl SecurityData {
    fn new() -> Self {
        if let Some(data) = Self::try_load() {
            log::info!("Loaded security data");
            data
        } else {
            log::info!("Created default security data");
            Self::default()
        }
    }

    fn try_load() -> Option<Self> { serde_json::from_slice(&std::fs::read(DATA_FILE_NAME).ok()?).ok() }

    fn save(&self) {
        std::fs::write(DATA_FILE_NAME, serde_json::to_vec_pretty(self).unwrap())
            .expect("Could not save security data");
    }

    fn set_seed_and_pin(
        &mut self,
        crypto: &CryptoApi,
        seed: &Seed,
        raw_pin: String,
        pin_entry: PinEntryMode,
    ) {
        self.seed = seed.to_vec();
        self.seed_fingerprint = seed_fingerprint(crypto, seed).unwrap().to_vec();
        self.raw_pin = raw_pin;
        self.pin_entry_mode = pin_entry.into();
        self.save();
    }
}

impl Default for Server {
    fn default() -> Self {
        let crypto = CryptoApi::default();
        let mut data = SecurityData::new();
        if data.raw_pin.is_empty() {
            let seed = Seed::from_bytes(&rand::random::<[u8; 32]>());
            data.set_seed_and_pin(&crypto, &seed, "123456".to_string(), PinEntryMode::Pin);
        }
        Self { data, crypto, logged_in: false }
    }
}

#[cfg(not(feature = "recovery-os"))]
impl server::ScalarEventSubscriptionHandler<SubscribeDiskEncryptionKeysReady> for Server {
    fn handle(
        &mut self,
        _msg: SubscribeDiskEncryptionKeysReady,
        subscriber: server::ScalarEventSubscriber<DiskEncryptionKeysReady>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        if let Err(e) = subscriber.send(&DiskEncryptionKeysReady) {
            log::warn!("DiskEncryptionKeysReady send failed: {e:?}");
        }
        Ok(())
    }
}

impl server::BlockingArchiveHandler<SetSeedAndPin> for Server {
    fn handle(
        &mut self,
        mut msg: SetSeedAndPin,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetSeedAndPin as server::BlockingArchive>::Response {
        // Validate PIN length before processing
        validate_raw_pin(&msg.pin.0)?;

        self.data.set_seed_and_pin(&self.crypto, &msg.seed, std::mem::take(&mut msg.pin.0), msg.pin_entry);
        std::thread::sleep(std::time::Duration::from_millis(4000));
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
        validate_raw_pin(&msg.pin.0)?;

        if !self.logged_in {
            log::info!("not logged in, request would fail on hardware: ChangePin");
        }

        self.data.raw_pin = std::mem::take(&mut msg.pin.0);
        self.data.pin_entry_mode = msg.pin_entry.into();
        self.data.save();
        Ok(())
    }
}

impl server::BlockingArchiveHandler<IsPinSet> for Server {
    fn handle(
        &mut self,
        _msg: IsPinSet,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <IsPinSet as server::BlockingArchive>::Response {
        Ok(self.data.raw_pin.len() > 0)
    }
}

impl server::BlockingArchiveHandler<Login> for Server {
    fn handle(
        &mut self,
        msg: Login,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Login as server::BlockingArchive>::Response {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if msg.pin.0 == self.data.raw_pin {
            self.logged_in = true;
            self.data.login_attempts = 0;
            self.data.save();
            Ok(())
        } else {
            log::warn!("Login failed.");

            self.data.login_attempts += 1;
            self.data.save();
            Err(LoginFailed { attempts_left: MAX_LOGIN_ATTEMPTS - self.data.login_attempts })
        }
    }
}

impl server::BlockingArchiveHandler<GetAttemptsRemaining> for Server {
    fn handle(
        &mut self,
        _msg: GetAttemptsRemaining,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetAttemptsRemaining as server::BlockingArchive>::Response {
        Ok(MAX_LOGIN_ATTEMPTS - self.data.login_attempts)
    }
}

impl server::BlockingArchiveHandler<GetFactoryResetCounter> for Server {
    fn handle(
        &mut self,
        _msg: GetFactoryResetCounter,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetFactoryResetCounter as server::BlockingArchive>::Response {
        Ok(self.data.factory_reset_counter)
    }
}

impl server::BlockingArchiveHandler<GetSeed> for Server {
    fn handle(
        &mut self,
        _msg: GetSeed,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetSeed as server::BlockingArchive>::Response {
        if !self.logged_in {
            log::info!("not logged in, request would fail on hardware: GetSeed");
        }
        if self.data.seed.is_empty() {
            Ok(None)
        } else {
            let seed = Seed::from_bytes(self.data.seed.as_slice());
            Ok(Some(seed))
        }
    }
}

impl server::BlockingArchiveHandler<SetSeed> for Server {
    fn handle(
        &mut self,
        msg: SetSeed,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetSeed as server::BlockingArchive>::Response {
        if !self.logged_in {
            log::info!("not logged in, request would fail on hardware: SetSeed");
        }

        self.data.seed = msg.0.to_vec();
        self.data.seed_fingerprint = seed_fingerprint(&self.crypto, &msg.0).unwrap().to_vec();
        self.data.save();
        Ok(())
    }
}

impl server::BlockingArchiveHandler<GetAppSeed> for Server {
    fn handle(
        &mut self,
        _msg: GetAppSeed,
        sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetAppSeed as server::BlockingArchive>::Response {
        if !self.logged_in {
            log::info!("not logged in, request would fail on hardware: GetAppSeed");
        }

        let sender_app_id =
            xous::get_app_id(sender).map_err(|_| AccessDenied)?.ok_or(AccessDenied)?.0.to_vec();
        let master_seed = self.data.seed.clone();
        let app_seed: [u8; 32] = self
            .crypto
            .hmac256(sender_app_id, master_seed)
            .map_err(|_| AccessDenied)?
            .try_into()
            .expect("incorrect slice length");
        Ok(AppSeed::new(app_seed))
    }
}

impl server::BlockingArchiveHandler<GetFirmwareTimestamp> for Server {
    fn handle(
        &mut self,
        _msg: GetFirmwareTimestamp,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetFirmwareTimestamp as server::BlockingArchive>::Response {
        Ok(FirmwareTimestamp(self.data.firmware_timestamp))
    }
}

impl server::BlockingArchiveHandler<SetFirmwareTimestamp> for Server {
    fn handle(
        &mut self,
        msg: SetFirmwareTimestamp,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetFirmwareTimestamp as server::BlockingArchive>::Response {
        if !self.logged_in {
            log::info!("not logged in, request would fail on hardware: SetFirmwareTimestamp");
        }

        self.data.firmware_timestamp = msg.0 .0;
        self.data.save();
        Ok(())
    }
}

impl server::BlockingScalarHandler<Logout> for Server {
    fn handle(
        &mut self,
        _msg: Logout,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Logout as server::BlockingScalar>::Response {
        self.logged_in = false;
    }
}

impl server::BlockingScalarHandler<LoggedIn> for Server {
    fn handle(
        &mut self,
        _msg: LoggedIn,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <LoggedIn as server::BlockingScalar>::Response {
        self.logged_in
    }
}

#[cfg(not(keyos))]
impl server::BlockingArchiveHandler<GetPin> for Server {
    fn handle(
        &mut self,
        _msg: GetPin,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetPin as server::BlockingArchive>::Response {
        self.data.raw_pin.clone()
    }
}

impl server::BlockingArchiveHandler<Lockout> for Server {
    fn handle(
        &mut self,
        _msg: Lockout,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <Lockout as server::BlockingArchive>::Response {
        if !self.logged_in {
            log::info!("not logged in, request would fail on hardware: Lockout");
        }

        self.data.factory_reset_counter = self.data.factory_reset_counter + 1;
        self.data.seed = vec![0; 32];
        self.data.seed_fingerprint = vec![0; 32];
        self.data.save();
        Ok(())
    }
}

impl server::BlockingArchiveHandler<SignWithSecurityCheckKey> for Server {
    fn handle(
        &mut self,
        _msg: SignWithSecurityCheckKey,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SignWithSecurityCheckKey as server::BlockingArchive>::Response {
        if !self.logged_in {
            log::info!("not logged in, request would fail on hardware: SignWithSecurityCheckKey");
        }

        Ok([1; 64])
    }
}

impl server::BlockingArchiveHandler<SignWithFidoKey> for Server {
    fn handle(
        &mut self,
        _msg: SignWithFidoKey,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SignWithFidoKey as server::BlockingArchive>::Response {
        if !self.logged_in {
            log::info!("not logged in, request would fail on hardware: SignWithFidoKey");
        }

        Ok([1; 64])
    }
}

impl server::BlockingArchiveHandler<GetFidoPubkey> for Server {
    fn handle(
        &mut self,
        _msg: GetFidoPubkey,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetFidoPubkey as server::BlockingArchive>::Response {
        if !self.logged_in {
            log::info!("not logged in, request would fail on hardware: GetFidoPubkey");
        }

        Ok([1; 64])
    }
}

impl server::BlockingArchiveHandler<GetSecurityWords> for Server {
    fn handle(
        &mut self,
        _msg: GetSecurityWords,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetSecurityWords as server::BlockingArchive>::Response {
        Ok([SecurityWord(0), SecurityWord(1)])
    }
}

impl server::BlockingArchiveHandler<SetAttempts> for Server {
    fn handle(
        &mut self,
        msg: SetAttempts,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetAttempts as server::BlockingArchive>::Response {
        self.data.login_attempts = msg.0;
        self.data.save();
    }
}

impl server::BlockingArchiveHandler<GetSeedFingerprint> for Server {
    fn handle(
        &mut self,
        _msg: GetSeedFingerprint,
        _sender: server::xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetSeedFingerprint as server::BlockingArchive>::Response {
        Ok(self.data.seed_fingerprint.as_slice().try_into().unwrap())
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
        // There is no SECURAM to read a flashed version from. The marker must still parse as
        // SemVer, and order below any release, because quantum-link sends it as current_version
        // in the firmware update checks.
        Ok(Some(OsVersionInfo {
            bootloader_version: *b"sim\0\0\0\0\0",
            keyos_version: *b"0.0.0-sim\0\0\0\0\0\0\0\0\0\0\0",
        }))
    }
}

impl server::BlockingArchiveHandler<ScChallenge> for Server {
    fn handle(
        &mut self,
        _msg: ScChallenge,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <ScChallenge as server::BlockingArchive>::Response {
        Ok(ScProof([0u8; ScProof::SIZE]))
    }
}

impl server::BlockingArchiveHandler<GetDeviceId> for Server {
    fn handle(
        &mut self,
        _msg: GetDeviceId,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetDeviceId as server::BlockingArchive>::Response {
        Ok(DeviceId([0u8; 32]))
    }
}

impl server::BlockingArchiveHandler<KeycardAuthenticityMac> for Server {
    fn handle(
        &mut self,
        _msg: KeycardAuthenticityMac,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <KeycardAuthenticityMac as server::BlockingArchive>::Response {
        Ok([0u8; 32])
    }
}

impl server::BlockingArchiveHandler<GetBluetoothChallengeSecret> for Server {
    fn handle(
        &mut self,
        _msg: GetBluetoothChallengeSecret,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> BluetoothChallengeSecret {
        BluetoothChallengeSecret { secret: Default::default(), sent: false }
    }
}

impl server::BlockingScalarHandler<SetBluetoothCheckSecretSent> for Server {
    fn handle(
        &mut self,
        _msg: SetBluetoothCheckSecretSent,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
    }
}

impl server::BlockingArchiveHandler<SetBluetoothDeviceId> for Server {
    fn handle(
        &mut self,
        _msg: SetBluetoothDeviceId,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
    }
}

impl server::BlockingArchiveHandler<GetRandom> for Server {
    fn handle(
        &mut self,
        _msg: GetRandom,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetRandom as server::BlockingArchive>::Response {
        let random_bytes = rand::random::<[u8; 32]>();
        Ok(random_bytes)
    }
}

impl server::BlockingArchiveHandler<GetPinEntryMode> for Server {
    fn handle(
        &mut self,
        _msg: GetPinEntryMode,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetPinEntryMode as server::BlockingArchive>::Response {
        self.data.pin_entry_mode.into()
    }
}

impl server::BlockingScalarHandler<GetMasterKeyState> for Server {
    fn handle(
        &mut self,
        _msg: GetMasterKeyState,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> MasterKeyState {
        let (seed_empty, pin_empty) = (self.data.seed.is_empty(), self.data.raw_pin.is_empty());

        match (seed_empty, pin_empty) {
            (true, false) => MasterKeyState::Erased,
            (_, true) => MasterKeyState::Onboarding,
            (false, false) => MasterKeyState::Normal,
        }
    }
}

impl server::BlockingArchiveHandler<GetBootloaderBuildDate> for Server {
    fn handle(
        &mut self,
        _msg: GetBootloaderBuildDate,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<Option<u64>, AccessDenied> {
        Ok(None)
    }
}

impl server::Server for Server {}
