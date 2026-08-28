// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use backup_shard::Shard;
use keycard::{
    error::{KeycardError, KeycardIdentifyError},
    messages::{
        CheckBackup, DetectKeycard, FormatKeycard, GenerateShards, IdentifyKeycard, KeycardId,
        LoadShardFromKeycard, LoadedShard, MasterSeedRestored, PopShard, PushShard, ResetShards,
        RestoreMasterSeed, SetShamirScheme, ShamirScheme, StoreShardToKeycard,
    },
};
use security::{DeviceId, Seed};
use server::{BlockingArchiveHandler, BlockingScalarHandler, Server, ServerContext};

crypto::use_api!();
nfc::use_api!();
security::use_api!();

#[derive(server::Server)]
#[name = "os/keycard"]
pub struct KeycardServer {
    security: Security,
    crypto: CryptoApi,
    nfc: NfcApi,
    current_device_id: DeviceId,
    expected_seed_fingerprint: [u8; 32],
    shards: Vec<Shard>,
    /// The active Shamir scheme (set via SetShamirScheme or auto-detected from first loaded shard)
    active_scheme: Option<ShamirScheme>,
    /// Expected timestamp for shard validation (set from first loaded shard or generation)
    expected_timestamp: Option<u32>,
}

const NFC_READ_TIMEOUT: Duration = Duration::from_millis(1000);
const NFC_WRITE_TIMEOUT: Duration = Duration::from_millis(3000);

/// Get the current Unix timestamp in seconds
fn current_unix_timestamp() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).expect("Time went backwards").as_secs() as u32
}

impl Server for KeycardServer {}

impl KeycardServer {
    #[cfg(keyos)]
    fn new() -> Result<Self, KeycardError> {
        let security = security::Security::default();
        let crypto = CryptoApi::default();
        let nfc = NfcApi::default();
        let current_device_id = loop {
            if let Ok(device_id) = security.device_id() {
                break device_id;
            } else {
                // retry later, the BT chip is not ready
                std::thread::sleep(Duration::from_millis(100));
            }
        };

        Ok(Self {
            security,
            crypto,
            nfc,
            shards: Vec::new(),
            current_device_id,
            expected_seed_fingerprint: [0; 32],
            active_scheme: None,
            expected_timestamp: None,
        })
    }

    fn reset(&mut self) {
        self.shards.clear();
        self.active_scheme = None;
        self.expected_seed_fingerprint = [0; 32];
        self.expected_timestamp = None;
    }

    fn set_shamir_scheme(&mut self, scheme: ShamirScheme) -> Result<(), KeycardError> {
        if scheme.threshold == 0 || scheme.threshold > scheme.share_count {
            return Err(KeycardError::InvalidScheme);
        }
        self.active_scheme = Some(scheme);
        log::debug!("Active scheme set to: {:?}", scheme);
        Ok(())
    }

    fn generate_shards(&mut self, with_magic_backup: bool) -> Result<(), KeycardError> {
        // Use active scheme or default to 2-of-3
        let scheme = self.active_scheme.unwrap_or(ShamirScheme::DEFAULT);

        // Clear shards but preserve the scheme
        self.shards.clear();
        self.expected_seed_fingerprint = [0; 32];
        self.active_scheme = Some(scheme);

        // Get current timestamp for all shards
        let timestamp = current_unix_timestamp();
        self.expected_timestamp = Some(timestamp);

        let Some(seed) = self.security.seed()? else {
            return Err(KeycardError::SeedMissing);
        };
        let seed_fingerprint = self.security.seed_fingerprint()?;
        let seed_shares = self.crypto.split_secret(seed.to_vec(), scheme.share_count, scheme.threshold)?;

        for (seed_shamir_share_index, seed_shamir_share) in seed_shares.into_iter().enumerate() {
            let mut shard = Shard::new(
                self.current_device_id.0,
                seed_fingerprint,
                seed_shamir_share,
                seed_shamir_share_index,
                with_magic_backup,
            );
            // Set V1-specific fields
            shard.set_scheme_threshold(scheme.threshold);
            shard.set_scheme_share_count(scheme.share_count);
            shard.set_timestamp(timestamp);
            self.shards.push(shard);
        }
        log::debug!(
            "Generated {} shards with scheme {:?} and timestamp {}",
            self.shards.len(),
            scheme,
            timestamp
        );
        Ok(())
    }

    fn pop_shard(&mut self) -> Result<Shard, KeycardError> {
        let shard = self.shards.pop().ok_or(KeycardError::NoShardLeft)?;
        log::debug!("Popped shard {} ({} left in pool)", shard.seed_shamir_share_index(), self.shards.len());
        if shard.part_of_magic_backup() {
            Ok(shard)
        } else {
            self.shards.push(shard);
            Err(KeycardError::NotMagicBackupShard)
        }
    }

    fn push_shard(&mut self, shard: Shard, accept_different_device_id: bool) -> Result<(), KeycardError> {
        if !shard.part_of_magic_backup() {
            return Err(KeycardError::NotMagicBackupShard);
        }
        if shard.seed_fingerprint() != &self.expected_seed_fingerprint {
            return Err(KeycardError::DifferentSeedFingerprint);
        }
        if !accept_different_device_id && shard.device_id() != &self.current_device_id.0 {
            return Err(KeycardError::DifferentDeviceId);
        }

        // A share index already in the pool means another shard (keycard or Envoy) already
        // carries this part: the backup set is invalid. Reject rather than admitting a
        // duplicate, which would otherwise corrupt reconstruction.
        let shard_index = shard.seed_shamir_share_index();
        if self.shards.iter().any(|s| s.seed_shamir_share_index() == shard_index) {
            return Err(KeycardError::DuplicateShardIndex { index: shard_index });
        }

        // Validate timestamp
        let shard_timestamp = shard.timestamp();
        if let Some(expected) = self.expected_timestamp {
            if shard_timestamp != expected {
                log::warn!("Pushed shard timestamp {} doesn't match expected {}", shard_timestamp, expected);
                return Err(KeycardError::TimestampMismatch { expected, found: shard_timestamp });
            }
        } else {
            // First shard being pushed - set expected timestamp
            self.expected_timestamp = Some(shard_timestamp);
        }

        self.shards.push(shard);
        log::debug!("Pushed shard {} ({} in pool)", shard_index, self.shards.len());
        Ok(())
    }

    fn identify_keycard(&mut self) -> Result<(Vec<u8>, Option<KeycardIdentifyError>), KeycardError> {
        let (uid, raw_msg) = self.nfc.read_ndef_raw_msg(NFC_READ_TIMEOUT)?;
        log::debug!("Read raw message ({} bytes)", raw_msg.len());
        let Ok(ndef_msg) = ndef::Message::try_from(raw_msg.as_slice()) else {
            return Ok((uid, Some(KeycardIdentifyError::InvalidData)));
        };
        if ndef_msg.records.len() != 1 {
            return Ok((uid, Some(KeycardIdentifyError::InvalidData)));
        }
        if !ndef_msg.records[0].is_type_cbor() {
            return Ok((uid, Some(KeycardIdentifyError::InvalidData)));
        }
        let Ok(payload) = ndef_msg.records[0].payload() else {
            return Ok((uid, Some(KeycardIdentifyError::InvalidData)));
        };
        let Ok(shard) = Shard::decode(&payload) else {
            return Ok((uid, Some(KeycardIdentifyError::InvalidData)));
        };
        if &hmac(&self.security, &shard, &uid)? != shard.hmac() {
            return Ok((uid, Some(KeycardIdentifyError::HmacMismatch)));
        }
        if shard.seed_shamir_share().is_empty() {
            return Ok((uid, None));
        }
        if shard.device_id() != &self.current_device_id.0 {
            return Ok((uid, Some(KeycardIdentifyError::DifferentDeviceId)));
        }
        if shard.seed_fingerprint() != &self.security.seed_fingerprint()? {
            return Ok((uid, Some(KeycardIdentifyError::DifferentSeedFingerprint)));
        }
        Ok((uid, Some(KeycardIdentifyError::ExistingShard)))
    }

    fn store_shard_to_keycard(&mut self, uid: Vec<u8>) -> Result<(), KeycardError> {
        let shard = self.shards.pop().ok_or(KeycardError::NoShardLeft)?;
        log::debug!("Storing shard {} ({} left in pool)", shard.seed_shamir_share_index(), self.shards.len());
        self.write_shard_to_keycard(shard.clone(), uid).map_err(|e| {
            // Put the shard back so the write can be retried.
            self.shards.push(shard);
            e
        })
    }

    fn format_keycard(&mut self, uid: Vec<u8>) -> Result<(), KeycardError> {
        // For a formatted card, only the HMAC must be valid; the rest of the fields can be zeroed and
        // the seed_shamir_share must be empty.
        self.write_shard_to_keycard(Shard::default(), uid)
    }

    fn write_shard_to_keycard(&mut self, mut shard: Shard, uid: Vec<u8>) -> Result<(), KeycardError> {
        shard.set_hmac(hmac(&self.security, &shard, &uid)?);
        let mut ndef_msg = ndef::Message::default();
        ndef_msg.append_record(ndef::Record::new(None, ndef::Payload::from_cbor_encodable(&shard)));
        let raw_msg = ndef_msg.to_vec().map_err(|_| KeycardError::Ndef)?;
        self.nfc.write_ndef_raw_msg(uid, raw_msg, NFC_WRITE_TIMEOUT).map_err(Into::into)
    }

    fn load_shard_from_keycard(&mut self) -> Result<LoadedShard, KeycardError> {
        let (uid, raw_msg) = self.nfc.read_ndef_raw_msg(NFC_READ_TIMEOUT)?;
        log::debug!("Load raw message ({} bytes)", raw_msg.len());
        if raw_msg.is_empty() {
            return Err(KeycardError::BlankTag);
        }
        let ndef_msg = ndef::Message::try_from(raw_msg.as_slice()).map_err(|_| KeycardError::Ndef)?;
        if ndef_msg.records.len() != 1 {
            return Err(KeycardError::InvalidData);
        }
        if !ndef_msg.records[0].is_type_cbor() {
            return Err(KeycardError::InvalidData);
        }
        let payload = ndef_msg.records[0].payload().map_err(|_| KeycardError::InvalidData)?;
        let shard = Shard::decode(&payload).map_err(|_| KeycardError::InvalidData)?;
        if &hmac(&self.security, &shard, &uid)? != shard.hmac() {
            return Err(KeycardError::HmacMismatch);
        }
        // Ignore formatted Keycard with blank Shard
        if shard.seed_shamir_share().is_empty() {
            return Err(KeycardError::BlankShard);
        }

        // Extract scheme and timestamp from shard (V1 has them, V0 falls back to defaults)
        let shard_scheme =
            ShamirScheme { threshold: shard.scheme_threshold(), share_count: shard.scheme_share_count() };
        let shard_timestamp = shard.timestamp();

        if self.shards.is_empty() {
            self.expected_seed_fingerprint = *shard.seed_fingerprint();
            // Auto-detect scheme from first shard if not already set
            if self.active_scheme.is_none() {
                self.active_scheme = Some(shard_scheme);
                log::debug!("Auto-detected scheme from first shard: {:?}", shard_scheme);
            }
            // Auto-detect timestamp from first shard if not already set
            if self.expected_timestamp.is_none() {
                self.expected_timestamp = Some(shard_timestamp);
                log::debug!("Auto-detected timestamp from first shard: {}", shard_timestamp);
            }
        } else {
            if &self.expected_seed_fingerprint != shard.seed_fingerprint() {
                return Err(KeycardError::DifferentSeedFingerprint);
            }
            // Validate scheme matches the active scheme
            if let Some(active) = self.active_scheme {
                if active != shard_scheme {
                    log::warn!("Shard scheme {:?} doesn't match active scheme {:?}", shard_scheme, active);
                    return Err(KeycardError::SchemeMismatch);
                }
            }
            // Validate timestamp matches
            if let Some(expected) = self.expected_timestamp {
                if shard_timestamp != expected {
                    log::warn!("Shard timestamp {} doesn't match expected {}", shard_timestamp, expected);
                    return Err(KeycardError::TimestampMismatch { expected, found: shard_timestamp });
                }
            }
        }

        let part_of_magic_backup = shard.part_of_magic_backup();
        // Callers dedup by physical UID before loading, so a share index that is already in
        // the pool comes from a different keycard carrying the same part: the backup set is
        // invalid (a healthy backup assigns a distinct index per shard). Reject it loudly
        // rather than dropping it silently, which would otherwise surface later as a
        // confusing NotEnoughShards at CheckBackup.
        let shard_index = shard.seed_shamir_share_index();
        if self.shards.iter().any(|s| s.seed_shamir_share_index() == shard_index) {
            return Err(KeycardError::DuplicateShardIndex { index: shard_index });
        }
        self.shards.push(shard);
        log::debug!("Shards in pool: {}", self.shards.len());
        Ok(LoadedShard {
            id: KeycardId(uid),
            has_magic_backup: part_of_magic_backup,
            seed_fingerprint: self.expected_seed_fingerprint,
            scheme: shard_scheme,
            timestamp: shard_timestamp,
        })
    }

    fn reconstruct_seed(&self) -> Result<MasterSeedRestored, KeycardError> {
        let mut indexes = Vec::new();
        let mut shares = Vec::new();
        let mut different_device_id = false;

        for s in &self.shards {
            if s.device_id() != &self.current_device_id.0 {
                different_device_id = true;
            }

            indexes.push(s.seed_shamir_share_index());
            shares.push(s.seed_shamir_share().to_vec());
        }

        let recovered = self.crypto.recover_secret(indexes, shares)?;
        let seed = Seed::from_bytes(&recovered);

        let seed_fingerprint = self.security.fingerprint(&seed)?;
        log::debug!("Restored master seed fingerprint: {:02x?}", seed_fingerprint);
        log::debug!("Expected master seed fingerprint: {:02x?}", self.expected_seed_fingerprint);
        if seed_fingerprint != self.expected_seed_fingerprint {
            return Err(KeycardError::DifferentSeedFingerprint);
        }
        Ok(MasterSeedRestored { seed, different_device_id })
    }

    fn check_backup(&mut self) -> Result<(), KeycardError> {
        let scheme = self.active_scheme.unwrap_or(ShamirScheme::DEFAULT);
        if self.shards.len() < scheme.share_count {
            return Err(KeycardError::NotEnoughShards);
        }

        let _master_seed_restored = self.reconstruct_seed()?;

        self.reset();
        Ok(())
    }

    fn restore_master_seed(&mut self) -> Result<MasterSeedRestored, KeycardError> {
        let scheme = self.active_scheme.unwrap_or(ShamirScheme::DEFAULT);
        if self.shards.len() < scheme.threshold {
            return Err(KeycardError::NotEnoughShards);
        }

        let master_seed_restored = self.reconstruct_seed()?;

        self.reset();
        Ok(master_seed_restored)
    }
}

fn hmac(security: &Security, shard: &Shard, uid: &[u8]) -> Result<[u8; 32], KeycardError> {
    let input = shard.hmac_input(uid);
    let input_hash = CryptoApi::default().sha256(&input)?;
    Ok(security.keycard_authenticity_mac(input_hash)?)
}

impl BlockingScalarHandler<ResetShards> for KeycardServer {
    fn handle(
        &mut self,
        _msg: ResetShards,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <ResetShards as server::BlockingScalar>::Response {
        self.reset();
        Ok(())
    }
}

impl BlockingArchiveHandler<SetShamirScheme> for KeycardServer {
    fn handle(
        &mut self,
        msg: SetShamirScheme,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <SetShamirScheme as server::BlockingArchive>::Response {
        self.set_shamir_scheme(msg.0)
    }
}

impl BlockingScalarHandler<GenerateShards> for KeycardServer {
    fn handle(
        &mut self,
        GenerateShards { with_magic_backup }: GenerateShards,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GenerateShards as server::BlockingScalar>::Response {
        self.generate_shards(with_magic_backup)
    }
}

impl BlockingArchiveHandler<PopShard> for KeycardServer {
    fn handle(
        &mut self,
        _msg: PopShard,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <PopShard as server::BlockingArchive>::Response {
        self.pop_shard()
    }
}

impl BlockingArchiveHandler<PushShard> for KeycardServer {
    fn handle(
        &mut self,
        msg: PushShard,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <PushShard as server::BlockingArchive>::Response {
        self.push_shard(msg.shard, msg.accept_different_device_id)
    }
}

impl BlockingArchiveHandler<FormatKeycard> for KeycardServer {
    fn handle(
        &mut self,
        msg: FormatKeycard,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <FormatKeycard as server::BlockingArchive>::Response {
        self.format_keycard(msg.0 .0)
    }
}

impl BlockingArchiveHandler<IdentifyKeycard> for KeycardServer {
    fn handle(
        &mut self,
        _msg: IdentifyKeycard,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <IdentifyKeycard as server::BlockingArchive>::Response {
        self.identify_keycard().map(|(uid, err)| (KeycardId(uid), err))
    }
}

impl BlockingArchiveHandler<StoreShardToKeycard> for KeycardServer {
    fn handle(
        &mut self,
        msg: StoreShardToKeycard,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <StoreShardToKeycard as server::BlockingArchive>::Response {
        self.store_shard_to_keycard(msg.0 .0)
    }
}

impl BlockingArchiveHandler<LoadShardFromKeycard> for KeycardServer {
    fn handle(
        &mut self,
        _msg: LoadShardFromKeycard,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <LoadShardFromKeycard as server::BlockingArchive>::Response {
        self.load_shard_from_keycard()
    }
}

impl BlockingScalarHandler<CheckBackup> for KeycardServer {
    fn handle(
        &mut self,
        _msg: CheckBackup,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <CheckBackup as server::BlockingScalar>::Response {
        self.check_backup()
    }
}

impl BlockingArchiveHandler<RestoreMasterSeed> for KeycardServer {
    fn handle(
        &mut self,
        _msg: RestoreMasterSeed,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <RestoreMasterSeed as server::BlockingArchive>::Response {
        self.restore_master_seed()
    }
}

impl BlockingArchiveHandler<DetectKeycard> for KeycardServer {
    fn handle(
        &mut self,
        msg: DetectKeycard,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <DetectKeycard as server::BlockingArchive>::Response {
        let (uid, _raw_msg) = self.nfc.read_ndef_raw_msg(msg.timeout)?;
        Ok(KeycardId(uid))
    }
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::AppBackground1).unwrap();

    #[cfg(keyos)]
    server::listen(KeycardServer::new().unwrap())
}
