// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::{Duration, Instant};

use file_backed::{FileBacked, JsonBacked, JsonCodec};
use gui_server_api::navigation::securitykeys::UserPresenceOptions;
use p256::ecdsa::{signature::Signer, Signature, SigningKey, VerifyingKey};
use server::{
    ArchiveEventSubscriptionHandler, ArchiveSubList, BlockingArchiveHandler, BlockingScalarHandler,
    MessageId as _, ScalarHandler, Server, ServerContext,
};
use sha2::{Digest, Sha256};

#[cfg(feature = "test-app")]
use crate::messages::ResetState;
use crate::{
    ctap::{PublicKeyCredentialRpEntity, PublicKeyCredentialUserEntity},
    error::FidoError,
    implementation::fs_permissions::FileSystemPermissions,
    messages::{
        CreateSecurityKey, CtapProcessCbor, EditSecurityKey, GetSelectedSecurityKey, ListSecurityKeys,
        OperationOutcomeEvent, SelectSecurityKey, SetArchived, SubscribeKeyChanges,
        SubscribeOperationOutcomes, SubscribePresenceKeepAlive, U2fProcessApdu,
    },
    nav_thread::NavThread,
    u2f::{Error as U2fError, KeyHandle, RegisterResponse},
    CryptoApi, RegisteredKey, RegisteredKeyCtap, RegisteredKeyU2f, SecurityKey, SecurityKeyView,
};

fs::use_api!();
security::use_api!();
settings::use_api!();

const STATE_FILE: &str = "security_keys_v1.json";
pub(crate) const SELECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of polling a pending presence check.
#[derive(Debug)]
pub(crate) enum PresencePoll {
    /// Prompt is in flight; the caller should return a "retry me" code to the RP.
    Pending,
    /// User confirmed. `selected_key_index` is the key the user picked in the modal, if any.
    Confirmed { selected_key_index: Option<usize> },
    /// User dismissed, or the GUI IPC failed.
    Dismissed,
}

/// State stored in a single `Arc<Mutex<_>>` shared between the FIDO main thread and the Nav
/// thread. Lives for the entire server lifetime — only the variant changes.
///
/// The fingerprint travels inside the variant so the Nav thread can drop a stale result on
/// the floor when FIDO has moved on (evicted the slot or started a different prompt).
///
/// `Pending` is a latest-wins inbox: FIDO writes the request payload here and signals Nav via
/// the paired `Condvar`. If FIDO overwrites a Pending entry before Nav has picked it up, the
/// older request is discarded — we never queue prompts.
#[derive(Debug)]
pub(crate) enum PresenceState {
    Idle,
    Pending { fingerprint: [u8; 32], options: UserPresenceOptions },
    InProgress { fingerprint: [u8; 32] },
    Completed { fingerprint: [u8; 32], present: bool, selected_key_index: Option<usize> },
}

/// SHA-256 of the request payload. Used to match retries to a pending prompt.
pub(crate) fn presence_fingerprint(payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    hasher.finalize().into()
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct FidoKeysState {
    pub security_keys: Vec<SecurityKey>,
}

impl FidoKeysState {
    fn security_key_mut(&mut self, index: usize) -> Result<&mut SecurityKey, FidoError> {
        self.security_keys.get_mut(index).ok_or(FidoError::InvalidIndex)
    }
}

#[derive(Debug, Default)]
pub struct FidoKey {
    signing_keys: Vec<SigningKey>,
    next: Option<(SigningKey, Vec<u8>)>,
}

/// Official attestation certificate (DER-encoded X.509)
const OFFICIAL_CERTIFICATE: [u8; 353] = [
    0x30, 0x82, 0x01, 0x5d, 0x30, 0x82, 0x01, 0x02, 0xa0, 0x03, 0x02, 0x01, 0x02, 0x02, 0x14, 0x68, 0x44,
    0x90, 0x6b, 0x09, 0x8e, 0x6c, 0x32, 0xe9, 0x4b, 0x03, 0xe5, 0x57, 0x46, 0x89, 0xf2, 0x93, 0xcb, 0xfd,
    0x89, 0x30, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02, 0x30, 0x2e, 0x31, 0x2c,
    0x30, 0x2a, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0c, 0x23, 0x46, 0x6f, 0x75, 0x6e, 0x64, 0x61, 0x74, 0x69,
    0x6f, 0x6e, 0x20, 0x44, 0x65, 0x76, 0x69, 0x63, 0x65, 0x73, 0x20, 0x46, 0x49, 0x44, 0x4f, 0x20, 0x41,
    0x74, 0x74, 0x65, 0x73, 0x74, 0x61, 0x74, 0x69, 0x6f, 0x6e, 0x30, 0x1e, 0x17, 0x0d, 0x32, 0x36, 0x30,
    0x31, 0x30, 0x31, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x5a, 0x17, 0x0d, 0x33, 0x36, 0x30, 0x31, 0x30,
    0x31, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x5a, 0x30, 0x2e, 0x31, 0x2c, 0x30, 0x2a, 0x06, 0x03, 0x55,
    0x04, 0x03, 0x0c, 0x23, 0x46, 0x6f, 0x75, 0x6e, 0x64, 0x61, 0x74, 0x69, 0x6f, 0x6e, 0x20, 0x44, 0x65,
    0x76, 0x69, 0x63, 0x65, 0x73, 0x20, 0x46, 0x49, 0x44, 0x4f, 0x20, 0x41, 0x74, 0x74, 0x65, 0x73, 0x74,
    0x61, 0x74, 0x69, 0x6f, 0x6e, 0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02,
    0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00, 0x04, 0xa8, 0x61,
    0xfe, 0xad, 0x21, 0xc2, 0xdc, 0x3e, 0xe9, 0x81, 0xb2, 0xbc, 0x27, 0x91, 0x33, 0x23, 0x83, 0xf0, 0x9e,
    0xe6, 0xce, 0x9f, 0x1e, 0x25, 0x00, 0x34, 0x46, 0x2c, 0xac, 0x12, 0xae, 0xfa, 0x03, 0x26, 0xff, 0xc2,
    0x3d, 0x2a, 0xf0, 0xe2, 0xe8, 0x87, 0xff, 0xf9, 0x05, 0x93, 0x08, 0xa7, 0x7f, 0x10, 0x69, 0x70, 0x5d,
    0xaf, 0x41, 0x4d, 0xb2, 0xb0, 0x6d, 0xcd, 0x35, 0x77, 0xc3, 0x58, 0x30, 0x0a, 0x06, 0x08, 0x2a, 0x86,
    0x48, 0xce, 0x3d, 0x04, 0x03, 0x02, 0x03, 0x49, 0x00, 0x30, 0x46, 0x02, 0x21, 0x00, 0xb2, 0xa4, 0x21,
    0x38, 0xab, 0x3a, 0x42, 0x7e, 0x5a, 0x98, 0xfb, 0x6d, 0x02, 0x46, 0x81, 0xe0, 0xfa, 0x30, 0x38, 0x81,
    0xb5, 0xbc, 0x19, 0xeb, 0x50, 0x30, 0x82, 0x12, 0x30, 0x1c, 0x30, 0x2d, 0x02, 0x21, 0x00, 0xc1, 0x28,
    0xd6, 0xb6, 0xb8, 0xc0, 0x32, 0xeb, 0xb0, 0x7c, 0x11, 0xf4, 0xd3, 0xe6, 0xd4, 0x94, 0x43, 0xa3, 0xfd,
    0x12, 0x92, 0x1b, 0x5d, 0xa8, 0x2c, 0x6d, 0x44, 0x41, 0x5c, 0x8b, 0xa8, 0x49,
];

/// Official attestation pubkey (65 bytes: 0x04 prefix + 64-byte uncompressed point)
const OFFICIAL_PUBKEY: [u8; 65] = [
    0x04, 0xa8, 0x61, 0xfe, 0xad, 0x21, 0xc2, 0xdc, 0x3e, 0xe9, 0x81, 0xb2, 0xbc, 0x27, 0x91, 0x33, 0x23,
    0x83, 0xf0, 0x9e, 0xe6, 0xce, 0x9f, 0x1e, 0x25, 0x00, 0x34, 0x46, 0x2c, 0xac, 0x12, 0xae, 0xfa, 0x03,
    0x26, 0xff, 0xc2, 0x3d, 0x2a, 0xf0, 0xe2, 0xe8, 0x87, 0xff, 0xf9, 0x05, 0x93, 0x08, 0xa7, 0x7f, 0x10,
    0x69, 0x70, 0x5d, 0xaf, 0x41, 0x4d, 0xb2, 0xb0, 0x6d, 0xcd, 0x35, 0x77, 0xc3, 0x58,
];

#[derive(server::Server)]
#[name = "os/fido"]
pub struct FidoServer {
    crypto: CryptoApi,
    pub(crate) aaguid: [u8; 16],
    pub(crate) state: JsonBacked<FidoKeysState, FileSystemPermissions>,
    pub(crate) attestation_certificate: Vec<u8>,
    pub(crate) attestation_pubkey: Vec<u8>,
    seed: Vec<u8>,
    fido_keys: Vec<FidoKey>,
    key_change_subscribers: ArchiveSubList<crate::messages::KeysChangedEvent>,
    presence_keep_alive_subscribers: ArchiveSubList<crate::messages::PresenceKeepAliveEvent>,
    pub(crate) operation_outcome_subscribers: ArchiveSubList<OperationOutcomeEvent>,
    /// Owns the shared presence-prompt slot, the worker thread, and the FIDO-side activity
    /// timestamp. All presence-poll logic flows through `nav.poll(...)`.
    nav: NavThread,
    /// Fingerprint of the last U2F APDU whose outcome we logged. Used to silence the
    /// "Register" / "Authenticate" + `Err(ConditionNotSatisfied)` pair on every retry
    /// during a user-presence polling loop.
    pub(crate) last_u2f_fingerprint: Option<[u8; 32]>,
    /// The key the user picked, and when. Lives outside [`FidoKeysState`] because it is
    /// selected mid-transaction, and persisting it there would put a flash write on the
    /// APDU path for something that never outlives the process.
    pub(crate) selected: Option<(usize, Instant)>,
}

/// Server-internal message dispatched by the kernel when a client process disconnects.
/// Routed to `ScalarHandler<DisconnectHandlerMessage>` below to prune dead subscribers.
#[derive(Debug, server::Message)]
pub(crate) struct DisconnectHandlerMessage(xous::CID);

impl Server for FidoServer {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        xous::register_system_event_handler(
            xous::SystemEvent::Disconnected,
            context.sid(),
            DisconnectHandlerMessage::ID,
        )
        .expect("register fido disconnect handler");
    }
}

impl ScalarHandler<DisconnectHandlerMessage> for FidoServer {
    fn handle(
        &mut self,
        DisconnectHandlerMessage(cid): DisconnectHandlerMessage,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.key_change_subscribers.remove_cid(cid);
        self.presence_keep_alive_subscribers.remove_cid(cid);
        self.operation_outcome_subscribers.remove_cid(cid);
    }
}

/// wait for:
/// - backup restore to complete
/// - secure element to unlock and give us our app_seed
/// - the AppData filesystem to mount (holds the persisted key state)
pub fn wait() -> (Security, [u8; 32]) {
    let settings = SettingsApi::default();
    settings.wait_for_onboarding_complete();

    let security = Security::default();
    let seed = security.app_seed().expect("app seed");

    // AppData mounts asynchronously on the same unlock that releases app_seed.
    FileSystem::default().wait_for_filesystem(fs::Location::AppData);

    (security, *seed.as_bytes())
}

impl FidoServer {
    pub fn new(security: Security, seed: [u8; 32]) -> Result<Self, FidoError> {
        log::info!("starting fido server");
        let state: FileBacked<JsonCodec<FidoKeysState>, _> =
            JsonBacked::new(STATE_FILE, fs::Location::AppData).0;
        log::debug!("Restored State: {:02x?}", state);

        // Get the SE's FIDO public key (64 bytes without 0x04 prefix)
        let se_pubkey = security
            .get_fido_pubkey()
            .inspect_err(|e| log::error!("security.get_fido_pubkey {e:?}"))
            .map_err(|_| FidoError::Other)?;

        // Check if SE pubkey matches the official pubkey (compare without 0x04 prefix)
        let (attestation_certificate, attestation_pubkey) = if se_pubkey[..] == OFFICIAL_PUBKEY[1..] {
            log::info!("Using official attestation certificate");
            (OFFICIAL_CERTIFICATE.to_vec(), OFFICIAL_PUBKEY.to_vec())
        } else {
            log::info!("Non-official pubkey detected, generating attestation certificate");
            let cert = crate::attestation_cert::build_attestation_certificate(&se_pubkey, |hash| {
                let sig = security.sign_with_fido_key(hash).map_err(|_| FidoError::Other)?;
                sig.try_into().map_err(|_| FidoError::Other)
            })?;

            // Build 65-byte pubkey with 0x04 prefix for non-official case
            let mut pubkey = Vec::with_capacity(65);
            pubkey.push(0x04);
            pubkey.extend_from_slice(&se_pubkey);
            (cert, pubkey)
        };

        let mut fido_server = Self {
            crypto: CryptoApi::default(),
            state,
            aaguid: [
                0x8f, 0x1b, 0xcc, 0xae, 0xeb, 0x8f, 0x12, 0xf8, 0x0b, 0x01, 0x7f, 0x55, 0x77, 0x4e, 0x3c,
                0xf5,
            ],
            attestation_certificate,
            attestation_pubkey,
            seed: seed.to_vec(),
            fido_keys: Vec::new(),
            key_change_subscribers: ArchiveSubList::default(),
            presence_keep_alive_subscribers: ArchiveSubList::default(),
            operation_outcome_subscribers: ArchiveSubList::default(),
            nav: NavThread::start(),
            last_u2f_fingerprint: None,
            selected: None,
        };
        fido_server.populate_fido_keys()?;
        fido_server.compute_next_signing_keys()?;
        log::debug!("FIDO Keys: {:02x?}", fido_server.fido_keys);
        Ok(fido_server)
    }

    fn populate_fido_keys(&mut self) -> Result<(), FidoError> {
        self.fido_keys = self
            .state
            .security_keys
            .iter()
            .enumerate()
            .map(|(security_key_index, security_key)| -> Result<FidoKey, FidoError> {
                let signing_keys = security_key
                    .registered_keys
                    .iter()
                    .enumerate()
                    .map(|(registered_key_index, _registered_key)| {
                        self.signing_key(security_key_index, registered_key_index)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(FidoKey { signing_keys, next: None })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(())
    }

    fn fido_key(&self, security_key_index: usize) -> Result<&FidoKey, FidoError> {
        self.fido_keys.get(security_key_index).ok_or(FidoError::InvalidIndex)
    }

    fn fido_key_mut(&mut self, security_key_index: usize) -> Result<&mut FidoKey, FidoError> {
        self.fido_keys.get_mut(security_key_index).ok_or(FidoError::InvalidIndex)
    }

    fn compute_next_signing_key_if_needed(&mut self, security_key_index: usize) -> Result<(), FidoError> {
        let needs_generation = self.fido_key(security_key_index)?.next.is_none();
        if needs_generation {
            let signing_keys_len =
                self.fido_key(security_key_index).map(|fido_key| fido_key.signing_keys.len()).unwrap_or(0);
            let next_signing_key = self.signing_key(security_key_index, signing_keys_len)?;
            let next_public_key = VerifyingKey::from(&next_signing_key).to_sec1_bytes().as_ref().to_vec();
            self.fido_key_mut(security_key_index)?.next = Some((next_signing_key, next_public_key));
        }
        Ok(())
    }

    fn compute_next_signing_keys(&mut self) -> Result<(), FidoError> {
        for i in 0..self.fido_keys.len() {
            self.compute_next_signing_key_if_needed(i)?;
        }
        Ok(())
    }

    fn use_next_signing_key(&mut self, security_key_index: usize) -> Result<Vec<u8>, FidoError> {
        self.compute_next_signing_key_if_needed(security_key_index)?;
        let fido_key = self.fido_key_mut(security_key_index)?;
        // below should never panic because the call to compute_next_signing_key_if_needed above
        // guarantee next.is_some()
        let next = fido_key.next.take().ok_or(FidoError::Other)?;
        fido_key.signing_keys.push(next.0);
        Ok(next.1)
    }

    /// Refresh the cached next signing keys and notify all subscribers of key changes.
    pub(crate) fn refresh_and_notify(&mut self) -> Result<(), FidoError> {
        self.compute_next_signing_keys()?;
        self.key_change_subscribers.send(&crate::messages::KeysChangedEvent { keys: self.key_views() });
        Ok(())
    }

    /// Build a list of SecurityKeyView for all keys.
    fn key_views(&self) -> Vec<SecurityKeyView> {
        self.state.security_keys.iter().enumerate().map(|(i, k)| k.to_view(i)).collect()
    }

    #[cfg(feature = "test-app")]
    fn reset_state(&mut self) -> Result<(), FidoError> {
        self.fido_keys = Vec::new();
        self.compute_next_signing_keys()?;
        self.state.guard().0 = FidoKeysState::default();
        self.selected = None;
        Ok(())
    }

    fn create_security_key(&mut self, label: String, color: u8, icon: String) -> Result<usize, FidoError> {
        log::debug!("creating new security key with label '{}'", label);
        if label.is_empty() {
            return Err(FidoError::EmptyLabel);
        }
        if !self.validate_label(None, &label) {
            return Err(FidoError::DuplicateLabel);
        }
        self.fido_keys.push(FidoKey::default());
        self.compute_next_signing_keys()?;
        let new_index = self.state.security_keys.len();
        let mut key = SecurityKey::default();
        key.label = label;
        key.color = color;
        key.icon = icon;
        key.live = true;
        key.date = system_time() as u64;
        let mut state = self.state.guard();
        state.security_keys.push(key);
        Ok(new_index)
    }

    fn edit_security_key(
        &mut self,
        index: usize,
        label: String,
        color: u8,
        icon: String,
        date: u64,
    ) -> Result<(), FidoError> {
        log::debug!("edit_security_key: index={}, label='{}', color={}, date={}", index, label, color, date);
        if label.is_empty() {
            return Err(FidoError::EmptyLabel);
        }
        if !self.validate_label(Some(index), &label) {
            return Err(FidoError::DuplicateLabel);
        }
        let mut state = self.state.guard();
        let key = state.security_key_mut(index)?;
        key.label = label;
        key.color = color;
        key.icon = icon;
        if date != 0 {
            key.date = date;
        }
        Ok(())
    }

    fn validate_label(&self, exclude_index: Option<usize>, label: &str) -> bool {
        if label.is_empty() {
            log::debug!("validate_label: empty label → invalid");
            return false;
        }
        let is_unique = !self.state.security_keys.iter().enumerate().any(|(i, k)| {
            if Some(i) == exclude_index {
                return false;
            }
            k.label == label
        });
        log::debug!(
            "validate_label: '{}' exclude={:?} → {}",
            label,
            exclude_index,
            if is_unique { "valid" } else { "duplicate" }
        );
        is_unique
    }

    fn select_security_key(&mut self, index: Option<usize>) -> Result<(), FidoError> {
        if let Some(idx) = index {
            if idx >= self.state.security_keys.len() {
                return Err(FidoError::InvalidIndex);
            }
        }
        let now = Instant::now();
        self.selected = index.map(|idx| (idx, now));
        Ok(())
    }

    /// Non-blocking user-presence check driven by RP retries.
    ///
    /// On first call for a given `fingerprint`, hands the request off to the long-lived Nav
    /// thread (which performs the blocking GUI IPC) and returns [`PresencePoll::Pending`]
    /// immediately so the RP can retry. Subsequent calls with the same fingerprint observe
    /// Nav's result via the shared `PresenceState` mutex and return [`PresencePoll::Confirmed`]
    /// or [`PresencePoll::Dismissed`].
    ///
    /// Mismatching fingerprints while a prompt is live also get `Pending` — we don't start a
    /// second modal or cancel the live one. A pending slot is only evicted when the RP that
    /// owns it has gone silent for longer than `SELECTION_TIMEOUT` (poll inactivity,
    /// not absolute age) and a different RP is now asking; the GUI-side keep-alive timeout
    /// handles closing an abandoned modal.
    pub(crate) fn poll_or_start_presence(
        &mut self,
        fingerprint: [u8; 32],
        options: UserPresenceOptions,
    ) -> PresencePoll {
        let verdict = self.nav.poll(fingerprint, options);

        // Emit a heartbeat whenever we tell the RP to retry. The Security Keys app uses this
        // to keep its modal alive; if the heartbeat stops (e.g. the RP stops polling), the app
        // auto-dismisses after a short inactivity window.
        if matches!(verdict, PresencePoll::Pending) {
            self.presence_keep_alive_subscribers
                .send(&crate::messages::PresenceKeepAliveEvent { fingerprint });
        }
        verdict
    }

    // TODO: only used in CTAP2 process, should be rethinked using Selected/Live attributes of SecurityKey
    pub(crate) fn security_key_index(
        &self,
        force_security_key_index: Option<usize>,
    ) -> Result<usize, FidoError> {
        Ok(force_security_key_index.unwrap_or(self.selected.ok_or(FidoError::UnselectedKey)?.0))
    }

    pub(crate) fn security_key(&self, index: usize) -> Result<&SecurityKey, FidoError> {
        self.state.security_keys.get(index).ok_or(FidoError::InvalidIndex)
    }

    /// Drives a U2F Register through peek → attest → commit in one shot. Attestation runs
    /// after the next signing key is primed but before the registered-key list is mutated,
    /// so a hashing or signing failure leaves on-disk and in-memory state aligned.
    pub(crate) fn attest_and_register_u2f(
        &mut self,
        security_key_index: usize,
        application_parameter: [u8; 32],
        challenge_parameter: [u8; 32],
    ) -> Result<Vec<u8>, U2fError> {
        let public_key = self.peek_next_registration_public_key(security_key_index)?;
        let registered_key_index = self.security_key(security_key_index)?.registered_keys.len();

        let key_handle = KeyHandle { security_key_index, registered_key_index };
        let mut resp = RegisterResponse::new(public_key, key_handle, self.attestation_certificate.clone());
        resp.attest(&application_parameter, &challenge_parameter)?;
        log::debug!("{resp:02x?}");
        let response_bytes = resp.to_vec();

        self.use_next_signing_key(security_key_index)?;
        let registered_timestamp = system_time();
        let mut state = self.state.guard();
        let security_key = state.security_key_mut(security_key_index)?;
        security_key.registered_keys.push(RegisteredKey::U2f(RegisteredKeyU2f {
            application_parameter,
            signature_counter: 0,
            registered_timestamp,
        }));
        Ok(response_bytes)
    }

    /// Returns the public key that the next registration would produce, without committing.
    /// Lets callers run fallible attestation before mutating registered-key state. Priming
    /// the cached next signing key here is idempotent and not part of the committed key
    /// list. Used by both U2F Register and CTAP MakeCredential.
    pub(crate) fn peek_next_registration_public_key(
        &mut self,
        security_key_index: usize,
    ) -> Result<Vec<u8>, FidoError> {
        self.compute_next_signing_key_if_needed(security_key_index)?;
        let public_key = self.fido_key(security_key_index)?.next.as_ref().ok_or(FidoError::Other)?.1.clone();
        Ok(public_key)
    }

    pub(crate) fn create_registered_key_ctap(
        &mut self,
        security_key_index: usize,
        rp: PublicKeyCredentialRpEntity,
        user: PublicKeyCredentialUserEntity,
    ) -> Result<(usize, Vec<u8>), FidoError> {
        let public_key = self.use_next_signing_key(security_key_index)?;
        let registered_timestamp = system_time();
        let mut state = self.state.guard();
        let security_key = state.security_key_mut(security_key_index)?;
        let new_resgistered_key_index = security_key.registered_keys.len();
        security_key.registered_keys.push(RegisteredKey::Ctap(RegisteredKeyCtap {
            rp,
            user,
            signature_counter: 0,
            registered_timestamp,
        }));
        Ok((new_resgistered_key_index, public_key))
    }

    fn signing_key(
        &self,
        security_key_index: usize,
        registered_key_index: usize,
    ) -> Result<SigningKey, FidoError> {
        let derivation_path =
            format!("m/83696968’/1179473391’/{}/{}", security_key_index, registered_key_index);
        // TODO: navigate to `settings` app to input the PIN if needed in order to login to `security` server
        // TODO: if we inputed the PIN, save it as auto UV
        let derivated_seed = self.crypto.hmac256(derivation_path.as_bytes().to_vec(), self.seed.clone())?;
        let signing_key = SigningKey::from_slice(&derivated_seed).map_err(|_| FidoError::Ecdsa)?;
        Ok(signing_key)
    }

    pub(crate) fn verifying_key_sec1(
        &self,
        security_key_index: usize,
        registered_key_index: usize,
    ) -> Result<Vec<u8>, FidoError> {
        let signing_key = self.signing_key(security_key_index, registered_key_index)?;
        let verifying_key = VerifyingKey::from(&signing_key).to_sec1_bytes().as_ref().to_vec();
        Ok(verifying_key)
    }

    pub(crate) fn sign_der(
        &mut self,
        security_key_index: usize,
        registered_key_index: usize,
        data: &[u8],
    ) -> Result<(Vec<u8>, u32), FidoError> {
        let signing_key = self
            .fido_key(security_key_index)?
            .signing_keys
            .get(registered_key_index)
            .ok_or(FidoError::InvalidIndex)?;
        let signature: Signature = signing_key.sign(data);
        let mut state = self.state.guard();
        let security_key = state.security_key_mut(security_key_index)?;
        let registered_key = security_key.registered_key_mut(registered_key_index)?;
        let signature_counter = registered_key.inc_signature_counter();
        Ok((signature.to_der().as_bytes().to_vec(), signature_counter))
    }
}

pub(crate) fn system_time() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).expect("Time went backwards").as_secs() as u32
}

// === Event subscription handler ===

impl ArchiveEventSubscriptionHandler<SubscribeKeyChanges> for FidoServer {
    fn handle(
        &mut self,
        _msg: SubscribeKeyChanges,
        subscriber: server::ArchiveEventSubscriber<crate::messages::KeysChangedEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        // Send the current key list as the initial event; only retain the subscriber if
        // the seed delivery succeeds (a failure here means the receiver is already gone).
        let event = crate::messages::KeysChangedEvent { keys: self.key_views() };
        if subscriber.send(&event).is_ok() {
            self.key_change_subscribers.push(subscriber);
        }
        Ok(())
    }
}

impl ArchiveEventSubscriptionHandler<SubscribePresenceKeepAlive> for FidoServer {
    fn handle(
        &mut self,
        _msg: SubscribePresenceKeepAlive,
        subscriber: server::ArchiveEventSubscriber<crate::messages::PresenceKeepAliveEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.presence_keep_alive_subscribers.push(subscriber);
        Ok(())
    }
}

impl ArchiveEventSubscriptionHandler<SubscribeOperationOutcomes> for FidoServer {
    fn handle(
        &mut self,
        _msg: SubscribeOperationOutcomes,
        subscriber: server::ArchiveEventSubscriber<OperationOutcomeEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.operation_outcome_subscribers.push(subscriber);
        Ok(())
    }
}

// === Key management handlers ===

impl BlockingArchiveHandler<CreateSecurityKey> for FidoServer {
    fn handle(
        &mut self,
        msg: CreateSecurityKey,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<usize, FidoError> {
        let index = self.create_security_key(msg.label, msg.color, msg.icon).inspect_err(|e| {
            log::warn!("create_security_key failed: {:?}", e);
        })?;
        log::debug!("security key created at index {index}");
        // The key itself is already persisted, so a failure here only costs the subscriber
        // notification and the cached next signing key.
        if let Err(e) = self.refresh_and_notify() {
            log::error!("failed to refresh keys after key creation: {:?}", e);
        }
        Ok(index)
    }
}

impl BlockingArchiveHandler<EditSecurityKey> for FidoServer {
    fn handle(
        &mut self,
        msg: EditSecurityKey,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), FidoError> {
        self.edit_security_key(msg.index, msg.label, msg.color, msg.icon, msg.date).inspect_err(|e| {
            log::warn!("edit_security_key failed: {:?}", e);
        })?;
        // Same as CreateSecurityKey: the edit is already persisted, only the notification is lost.
        if let Err(e) = self.refresh_and_notify() {
            log::error!("failed to refresh keys after key edit: {:?}", e);
        }
        Ok(())
    }
}

impl BlockingScalarHandler<SetArchived> for FidoServer {
    fn handle(
        &mut self,
        msg: SetArchived,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), FidoError> {
        log::debug!("SetArchived: index={}, archived={}", msg.index, msg.archived);
        let mut state = self.state.guard();
        let key = state.security_key_mut(msg.index).inspect_err(|e| {
            log::warn!("set_archived failed: {:?}", e);
        })?;
        key.set_archived(msg.archived);
        drop(state);
        // Same as CreateSecurityKey: the change is already persisted, only the notification is lost.
        if let Err(e) = self.refresh_and_notify() {
            log::error!("failed to refresh keys after set_archived: {:?}", e);
        }
        Ok(())
    }
}

impl BlockingArchiveHandler<ListSecurityKeys> for FidoServer {
    fn handle(
        &mut self,
        _msg: ListSecurityKeys,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> Vec<SecurityKeyView> {
        self.key_views()
    }
}

// === Selection handlers ===

impl BlockingScalarHandler<GetSelectedSecurityKey> for FidoServer {
    fn handle(
        &mut self,
        _msg: GetSelectedSecurityKey,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> Option<usize> {
        self.selected.map(|(index, _)| index).clone()
    }
}

impl ScalarHandler<SelectSecurityKey> for FidoServer {
    fn handle(&mut self, msg: SelectSecurityKey, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        if let Err(e) = self.select_security_key(msg.0) {
            log::warn!("select_security_key failed: {}", e);
        }
    }
}

// === Protocol handlers ===

impl BlockingArchiveHandler<U2fProcessApdu> for FidoServer {
    fn handle(
        &mut self,
        msg: U2fProcessApdu,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <U2fProcessApdu as server::BlockingArchive>::Response {
        self.u2f_process_apdu(&msg.command, msg.transport)
    }
}

impl BlockingArchiveHandler<CtapProcessCbor> for FidoServer {
    fn handle(
        &mut self,
        msg: CtapProcessCbor,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> <CtapProcessCbor as server::BlockingArchive>::Response {
        self.ctap_process_cbor(msg.cmd, &msg.raw)
    }
}

#[cfg(feature = "test-app")]
impl BlockingScalarHandler<ResetState> for FidoServer {
    fn handle(
        &mut self,
        _msg: ResetState,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), FidoError> {
        self.reset_state()?;
        Ok(())
    }
}
