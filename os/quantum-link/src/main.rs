// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod backoff;
mod bt_rx_bridge;
mod bt_tx_bridge;
mod pending_requests;
mod persist;
mod prestart;
mod state;
mod status;
mod subscriptions;

use std::sync::mpsc;

use anyhow::Context;
use chrono::{DateTime, Utc};
use foundation_api::{
    backup::*,
    bc_components::ARID,
    bc_envelope::{Envelope, EventBehavior, Expression},
    bc_xid::XIDDocument,
    bitcoin::{AccountUpdate, SignPsbt},
    dcbor::{CBOREncodable, CBOR},
    firmware::*,
    fx::{ExchangeRate, ExchangeRateHistory, PrimeFiatPreference},
    message::{EnvoyMessage, PassportMessage, QuantumLinkMessage, PROTOCOL_VERSION},
    onboarding::OnboardingState,
    pairing::{PairingRequest, UnpairingRequest},
    passport::PassportColor,
    quantum_link::{ARIDCache, QlError, QuantumLink, ReplayCheck, EXPIRATION_DURATION},
    scv::{ChallengeResponseResult, SecurityCheck},
    status::{DeviceNameUpdate, DeviceStatus, EnvoyStatus, Heartbeat, TimezoneRequest},
};
use gstp::{SealedEvent, SealedEventBehavior};
use log::debug;
use quantum_link::{messages::*, PairingEvent, SecurityCheckState, SendMessageError};
use security::OsVersionInfo;
use server::{
    xous::{self, PID},
    ArchiveEventSubscriber, ArchiveRequest, Owned, ServerContext,
};
use settings::global::EnvoyTimeSync;
use xous_ticktimer::{TicktimerCallback, TicktimerPrivileged};

use crate::{
    bt_rx_bridge::{BtReceptionBridge, BtRecvWake},
    bt_tx_bridge::{start_ble_send_thread, BtSend, BtSendFailure, HeartbeatSendResult, SendOutcome},
    pending_requests::{PendingRequest, PendingRequestKind, PendingRequests},
    persist::FileBacked,
    state::{PairedDevice, QuantumLinkState},
    status::{HeartbeatState, HeartbeatTick},
    subscriptions::MessageSubscribers,
};

fs::use_api!();
bt::use_api!();
power_manager::use_ext_api!();
security::use_api!();
settings::use_api!();

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System4).unwrap();

    // Wait for either fs, or a message from onboarding.
    server::listen(prestart::QuantumLinkPrestartServer);

    let (tx, rx) = std::sync::mpsc::channel();
    BtReceptionBridge::recv_bridge(tx);
    let state = state::QuantumLinkState::new();
    server::listen_with(|sid| QuantumLinkServer::new(sid, state, rx));
}

#[derive(server::Server)]
#[name = "os/quantum-link"]
pub struct QuantumLinkServer {
    #[allow(unused)]
    sid: xous::SID,
    state: FileBacked<QuantumLinkState>,
    fs: FileSystem,
    security: Security,
    settings: SettingsApi,

    tick_timer: TicktimerPrivileged,
    should_set_system_time: bool,

    bt_sender: mpsc::Sender<BtSend>,
    bt_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    arid_cache: ARIDCache,

    message_subscribers: MessageSubscribers,
    pending: PendingRequests,

    last_received_name: Option<String>,
    pwr_state: Option<power_manager::Status>,
    os_version: Option<OsVersionInfo>,

    last_status: quantum_link::ConnectionStatus,
    bt_state: bt::State,
    heartbeat_cb: xous_ticktimer::TicktimerCallback,
    heartbeat_state: HeartbeatState,
    missed_heartbeats: u32,
}

struct UnsealedEnvoyMessage {
    message: EnvoyMessage,
    sender: XIDDocument,
    arid: ARID,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone, server::Permissions)]
#[server_name = "os/quantum-link"]
#[all_permissions]
struct InternalPermissions;

impl server::Server for QuantumLinkServer {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        debug!("Subscribing to BLE messages");
        let mut bt = BluetoothApi::default();
        bt.subscribe_ble_state(context);

        let pwr = PowerManagerExtApi::default();
        pwr.subscribe_status(context);

        self.fs.subscribe_filesystem_events(context, fs::Location::AppData);
        self.settings.server_subscribe_envoy_time_sync(context);
        self.settings.server_subscribe_onboarding_status(context);
        self.settings.server_subscribe_device_name(context);
    }
}

impl QuantumLinkServer {
    fn is_paired(&mut self, sender: &XIDDocument) -> bool {
        self.state.guard().paired_device.as_ref().map(|d| &d.xid) == Some(sender)
    }

    pub fn new(
        sid: xous::SID,
        state: FileBacked<QuantumLinkState>,
        bt_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    ) -> Self {
        let bt_sender = start_ble_send_thread();
        let tick_timer = TicktimerPrivileged::default();
        let heartbeat_cb = TicktimerCallback::new(sid).unwrap();

        Self {
            sid,
            state,
            fs: FileSystem::default(),
            security: Security::default(),
            settings: SettingsApi::default(),

            tick_timer,
            should_set_system_time: true,

            bt_sender,
            arid_cache: ARIDCache::default(),

            bt_state: bt::State::Booting,
            pwr_state: None,
            os_version: None,

            message_subscribers: MessageSubscribers::default(),

            pending: PendingRequests::default(),
            last_received_name: None,
            bt_rx,

            heartbeat_cb,
            heartbeat_state: HeartbeatState::DEAD,
            missed_heartbeats: 0,
            last_status: quantum_link::ConnectionStatus {
                bt_connected: false,
                ql_paired: false,
                live: false,
            },
        }
    }

    fn handle_message(&mut self, message: &[u8]) -> anyhow::Result<()> {
        let cbor = CBOR::try_from_data(message).context("invalid cbor")?;
        let envelope = Envelope::try_from_cbor(cbor).context("invalid envelope")?;

        let unsealed = self.unseal_envoy_message(&envelope).context("unseal envelope")?;
        let message = &unsealed.message.message;
        let sender = &unsealed.sender;

        if let QuantumLinkMessage::PairingRequest(request) = message {
            return self.handle_pairing_request(request, sender, unsealed.message.timestamp);
        }

        if !self.is_paired(sender) {
            anyhow::bail!("Not paired with this sender. Need to send a pairing request first.");
        }

        match (message, self.heartbeat_state.time_sync) {
            (
                QuantumLinkMessage::Heartbeat(Heartbeat {
                    request_id: Some(response_id),
                    timestamp_ms: Some(timestamp_ms),
                }),
                Some(request_id),
            ) => {
                if *response_id == request_id {
                    self.heartbeat_state.time_sync = None;
                    self.sync_system_time(*timestamp_ms)?;
                } else {
                    log::warn!("heartbeat time sync request ID mismatch");
                }
            }
            _ => {}
        }
        self.check_replay(&unsealed).context("replay check")?;

        log_message("received message", &message);
        self.dispatch_ql_message(unsealed.message.message);

        Ok(())
    }

    fn handle_pairing_request(
        &mut self,
        request: &PairingRequest,
        sender: &XIDDocument,
        timestamp_seconds: u32,
    ) -> anyhow::Result<()> {
        if self.state.guard().paired_device.is_some() {
            anyhow::bail!("Already paired. Pairing request ignored.");
        }

        log::info!("received pairing request");

        let event = PairingEvent::RequestReceived;
        self.message_subscribers.pairing_event.send_nowait(&event);

        let pair_result = (|| {
            let xid_cbor = CBOR::try_from_data(request.clone().xid_document).context("invalid xid cbor")?;
            let xid_document = XIDDocument::try_from(xid_cbor).context("invalid xid")?;
            if &xid_document != sender {
                anyhow::bail!("pairing request XID did not match envelope sender");
            }

            self.sync_system_time(u64::from(timestamp_seconds) * 1_000)?;
            self.state.guard().paired_device =
                Some(PairedDevice { xid: xid_document, name: request.device_name.clone() });

            self.send_pairing_response()
        })();

        match pair_result {
            Ok(()) => {
                let event =
                    PairingEvent::PairingComplete { device_name: request.device_name.clone(), new: true };
                self.message_subscribers.pairing_event.send_nowait(&event);
                self.publish_device_name();
                // Restart the heartbeat loop, which goes dead while unpaired.
                self.missed_heartbeats = 0;
                self.heartbeat_tick();
                Ok(())
            }
            Err(e) => {
                log::error!("failed to pair device {e:?}");
                let event = PairingEvent::PairingFailed;
                self.message_subscribers.pairing_event.send_nowait(&event);
                Err(e)
            }
        }
    }

    fn unseal_envoy_message(&mut self, envelope: &Envelope) -> anyhow::Result<UnsealedEnvoyMessage> {
        let event: SealedEvent<Expression> = SealedEvent::try_from_envelope(
            envelope,
            None,
            None,
            &self.state.guard().system_identity.private_keys,
        )?;
        let expression = event.content().clone();
        let expires_at = event.date().ok_or(QlError::MissingDate)?.datetime();
        Ok(UnsealedEnvoyMessage {
            message: EnvoyMessage::decode(&expression)?,
            sender: event.sender().clone(),
            arid: event.id(),
            expires_at,
        })
    }

    fn check_replay(&mut self, message: &UnsealedEnvoyMessage) -> Result<(), QlError> {
        const CLOCK_SKEW_TOLERANCE: std::time::Duration = std::time::Duration::from_secs(30);
        let now = Utc::now();
        let expires_at = message.expires_at;
        if now >= expires_at {
            return Err(QlError::Expired);
        }
        if expires_at > now + EXPIRATION_DURATION + CLOCK_SKEW_TOLERANCE {
            return Err(QlError::FutureDated);
        }

        match self.arid_cache.check_and_store(&message.arid, expires_at, now) {
            ReplayCheck::Fresh => Ok(()),
            ReplayCheck::Replay => Err(QlError::ReplayAttack),
            ReplayCheck::Expired => Err(QlError::Expired),
        }
    }

    fn sync_system_time(&self, timestamp_ms: u64) -> anyhow::Result<()> {
        let timestamp_ns = timestamp_ms.checked_mul(1_000_000).context("system time overflow")?;
        if self.should_set_system_time {
            self.tick_timer.set_system_time(timestamp_ns);
        }
        Ok(())
    }

    fn send(&mut self, msg: QuantumLinkMessage, outcome: SendOutcome) -> Result<(), SendMessageError> {
        let version = self.get_current_os_version().unwrap_or_default();
        let battery_level = self.pwr_state.map(|p| p.battery_percent).unwrap_or(50);
        let state = self.state.guard();
        let device = state.paired_device.as_ref().ok_or(SendMessageError::NoDevicePaired)?;

        log_message("sending message", &msg);

        let status = DeviceStatus { version, battery_level };
        let message = PassportMessage { message: msg, status, protocol_version: Some(PROTOCOL_VERSION) };

        let envelope = QuantumLink::seal(
            message,
            (&state.system_identity.private_keys, &state.system_identity.xid_document),
            &device.xid,
        );
        let payload = envelope.to_cbor_data();

        self.bt_sender.send(BtSend { payload, outcome }).expect("Sender thread not running");
        Ok(())
    }

    fn send_pairing_response(&mut self) -> anyhow::Result<()> {
        use foundation_api::passport::{PassportFirmwareVersion, PassportModel, PassportSerial};

        let device_id = self.security.device_id().context("failed to get device id")?.to_string();

        let version_info =
            self.get_current_os_version().ok_or_else(|| anyhow::anyhow!("missing os version"))?;

        let passport_color = match self.settings.get_prime_color() {
            settings::global::SystemTheme::Dark => PassportColor::Dark,
            settings::global::SystemTheme::Light => PassportColor::Light,
        };
        let onboarding_complete = match self.settings.get_onboarding_status() {
            Some(settings::global::OnboardingStatus::Complete) => true,
            _ => false,
        };
        let pin_set = self.security.is_pin_set().unwrap_or(false);
        let device_name = self.settings.get_device_name().0;

        let response = foundation_api::pairing::PairingResponse {
            passport_model: PassportModel::Prime,
            passport_firmware_version: PassportFirmwareVersion(version_info),
            passport_serial: PassportSerial(device_id),
            passport_color,
            onboarding_complete: onboarding_complete && pin_set,
            device_name: Some(device_name),
        };

        log::info!("sending pairing response {response:?}");

        self.send(QuantumLinkMessage::PairingResponse(response), SendOutcome::Ignore)?;

        Ok(())
    }

    fn dispatch_ql_message(&mut self, msg: QuantumLinkMessage) {
        self.heartbeat_success();
        match msg {
            QuantumLinkMessage::ExchangeRate(rate) => {
                self.message_subscribers.exchange_rate.send_nowait(&rate);
            }
            QuantumLinkMessage::ExchangeRateHistory(history) => {
                self.message_subscribers.exchange_rate_history.send_nowait(&history);
            }
            QuantumLinkMessage::EnvoyStatus(status) => {
                self.message_subscribers.envoy_status.send_nowait(&status);
            }
            QuantumLinkMessage::DeviceNameUpdate(update) => {
                log::info!("received device name update: {}", update.device_name);
                self.handle_inbound_device_name(update);
            }
            QuantumLinkMessage::SignPsbt(psbt) => {
                self.message_subscribers.sign_psbt.send_nowait(&psbt);
            }
            QuantumLinkMessage::AccountUpdate(update) => {
                self.message_subscribers.account_update.send_nowait(&update);
            }
            QuantumLinkMessage::OnboardingState(state) => {
                self.message_subscribers.onboarding_state.send_nowait(&state);
            }
            QuantumLinkMessage::PairingRequest(_) => {
                // Auto-handled in handle_packet, no subscription forwarding needed
            }
            QuantumLinkMessage::SecurityCheck(s) => match s {
                SecurityCheck::ChallengeRequest(request) => {
                    self.handle_security_challenge_request(request);
                }
                SecurityCheck::ChallengeResponse(_) => {}
                SecurityCheck::VerificationResult(result) => {
                    log::info!("security verification result {result:?}");
                    let msg = match result {
                        foundation_api::scv::VerificationResult::Success => SecurityCheckState::Success,
                        foundation_api::scv::VerificationResult::Failure => SecurityCheckState::Failed,
                        foundation_api::scv::VerificationResult::Error { .. } => SecurityCheckState::Error,
                    };
                    self.message_subscribers.security_check_state.send_nowait(&msg);
                }
            },
            QuantumLinkMessage::FirmwareFetchEvent(response) => {
                self.handle_firmware_fetch_event(response);
                if let Some(request) = self.pending.update_start.take() {
                    request.respond(Ok(()));
                }
            }
            QuantumLinkMessage::FirmwareUpdateCheckResponse(response) => {
                if let Some(request) = self.pending.update_check.take() {
                    let available = match response {
                        FirmwareUpdateCheckResponse::Available(firmware_update_available) => {
                            Some(firmware_update_available)
                        }
                        FirmwareUpdateCheckResponse::NotAvailable => None,
                    };
                    request.respond(Ok(available));
                }
            }
            QuantumLinkMessage::EnvoyMagicBackupEnabledResponse(response) => {
                if let Some(request) = self.pending.envoy_magic_backup_enabled.take() {
                    request.respond(Ok(response.enabled));
                }
            }
            QuantumLinkMessage::BackupShardResponse(response) => {
                if let Some(request) = self.pending.backup_shard.take() {
                    request.respond(Ok(response));
                }
            }
            QuantumLinkMessage::RestoreShardResponse(response) => {
                if let Some(request) = self.pending.restore_shard.take() {
                    request.respond(Ok(response));
                }
            }
            QuantumLinkMessage::CreateMagicBackupResult(result) => {
                if let Some(request) = self.pending.create_magic_backup_result.take() {
                    request.respond(result);
                }
            }
            QuantumLinkMessage::RestoreMagicBackupEvent(event) => {
                self.message_subscribers.restore_magic_backup.send_nowait(&event);
                if let Some(request) = self.pending.restore_magic_backup.take() {
                    request.respond(Ok(()));
                }
            }
            QuantumLinkMessage::PrimeMagicBackupStatusResponse(response) => {
                if let Some(request) = self.pending.prime_magic_backup_status_response.take() {
                    request.respond(Ok(response));
                }
            }
            QuantumLinkMessage::Heartbeat(_) => {}
            QuantumLinkMessage::TimezoneResponse(response) => {
                if let Some(request) = self.pending.timezone.take() {
                    request.respond(Ok(response));
                }
            }
            QuantumLinkMessage::UnpairingResponse(_) => {}

            QuantumLinkMessage::DeviceStatus(_)
            | QuantumLinkMessage::PairingResponse(_)
            | QuantumLinkMessage::FirmwareUpdateCheckRequest(_)
            | QuantumLinkMessage::FirmwareFetchRequest(_)
            | QuantumLinkMessage::FirmwareInstallEvent(_)
            | QuantumLinkMessage::BackupShardRequest(_)
            | QuantumLinkMessage::RestoreShardRequest(_)
            | QuantumLinkMessage::EnvoyMagicBackupEnabledRequest(_)
            | QuantumLinkMessage::BroadcastTransaction(_)
            | QuantumLinkMessage::CreateMagicBackupEvent(_)
            | QuantumLinkMessage::RestoreMagicBackupRequest(_)
            | QuantumLinkMessage::RestoreMagicBackupResult(_)
            | QuantumLinkMessage::ApplyPassphrase(_)
            | QuantumLinkMessage::PrimeMagicBackupEnabled(_)
            | QuantumLinkMessage::PrimeFiatPreference(_)
            | QuantumLinkMessage::PrimeMagicBackupStatusRequest(_)
            | QuantumLinkMessage::UnpairingRequest(_)
            | QuantumLinkMessage::MagicBackupRequestV2(_)
            | QuantumLinkMessage::MagicBackupResponseV2(_)
            | QuantumLinkMessage::TimezoneRequest(_) => {
                log::warn!("received spurious event message");
                log::debug!("{msg:?}");
            }
        }
    }

    fn handle_firmware_fetch_event(&mut self, event: FirmwareFetchEvent) {
        self.message_subscribers.firmware_fetch.send_nowait(&event);
    }

    fn handle_inbound_device_name(&mut self, update: DeviceNameUpdate) {
        self.last_received_name = Some(update.device_name.clone());
        self.settings.set_device_name(settings::global::DeviceName(update.device_name));
    }

    fn publish_device_name(&mut self) {
        if self.state.guard().paired_device.is_none() {
            return;
        }
        let device_name = self.settings.get_device_name().0;
        if self.last_received_name.as_deref() == Some(device_name.as_str()) {
            return;
        }
        let update = DeviceNameUpdate { device_name };
        if let Err(e) = self.send(QuantumLinkMessage::DeviceNameUpdate(update), SendOutcome::Ignore) {
            log::warn!("failed to publish device name {e:?}");
        }
    }

    fn handle_security_challenge_request(&mut self, req: foundation_api::scv::ChallengeRequest) {
        log::info!("received security challenge request");
        let challenge_data = req.data;
        self.notify_security_check_state(SecurityCheckState::ReceivedChallenge);

        let mut challenge = [0; security::messages::ScChallenge::SIZE];
        let len = std::cmp::min(challenge_data.len(), security::messages::ScChallenge::SIZE);
        challenge.copy_from_slice(&challenge_data[..len]);

        match self.security.sc_challenge(challenge) {
            Ok(proof) => {
                log::info!("security challenge proof success");
                let proof = Vec::from(proof.0);
                let message = ChallengeResponseResult::Success { data: proof };
                let response = SecurityCheck::ChallengeResponse(message);

                if let Err(e) = self.send(QuantumLinkMessage::SecurityCheck(response), SendOutcome::Ignore) {
                    log::error!("failed to send security proof response: {:?}", e);
                    self.notify_security_check_state(SecurityCheckState::Failed);
                }
            }
            Err(e) => {
                log::error!("security challenge failed: {e:?}");
                let response =
                    ChallengeResponseResult::Error { error: format!("security challenge failed: {e:?}") };

                self.notify_security_check_state(SecurityCheckState::Failed);
                self.send(
                    QuantumLinkMessage::SecurityCheck(SecurityCheck::ChallengeResponse(response)),
                    SendOutcome::Ignore,
                )
                .ok();
            }
        }
    }

    fn notify_security_check_state(&mut self, state: SecurityCheckState) {
        self.message_subscribers.security_check_state.send_nowait(&state);
    }

    fn get_current_os_version(&mut self) -> Option<String> {
        fn version_to_string(info: &OsVersionInfo) -> String {
            let v = info.keyos_version;
            let len = v.iter().position(|&b| b == 0).unwrap_or(v.len());
            String::from_utf8_lossy(&v[..len]).into_owned()
        }

        if let Some(info) = &self.os_version {
            return Some(version_to_string(info));
        }

        match self.security.os_version_info() {
            Ok(Some(info)) => {
                let v = version_to_string(&info);
                self.os_version = Some(info);
                Some(v)
            }
            Ok(None) | Err(_) => {
                log::error!("failed to get os version");
                None
            }
        }
    }

    fn get_seed_fingerprint(&self) -> Option<[u8; 32]> {
        self.security.seed_fingerprint().inspect_err(|_| log::warn!("failed to fetch seed fingerprint")).ok()
    }
}

impl server::BlockingArchiveHandler<GetXidDocument> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: GetXidDocument,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetXidDocument as server::BlockingArchive>::Response {
        self.state.guard().system_identity.xid_document.to_cbor_data()
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeExchangeRate> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeExchangeRate,
        subscriber: ArchiveEventSubscriber<ExchangeRate>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.exchange_rate.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeExchangeRateHistory> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeExchangeRateHistory,
        subscriber: ArchiveEventSubscriber<ExchangeRateHistory>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.exchange_rate_history.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeEnvoyStatus> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeEnvoyStatus,
        subscriber: ArchiveEventSubscriber<EnvoyStatus>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.envoy_status.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeSignPsbt> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeSignPsbt,
        subscriber: ArchiveEventSubscriber<SignPsbt>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.sign_psbt.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeAccountUpdate> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeAccountUpdate,
        subscriber: ArchiveEventSubscriber<AccountUpdate>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.account_update.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribePublishedAccountUpdate> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribePublishedAccountUpdate,
        subscriber: ArchiveEventSubscriber<SendAccountUpdate>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.published_account_update.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeOnboardingState> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeOnboardingState,
        subscriber: ArchiveEventSubscriber<OnboardingState>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.onboarding_state.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeSecurityCheckState> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeSecurityCheckState,
        subscriber: ArchiveEventSubscriber<SecurityCheckState>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.security_check_state.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeFirmwareFetch> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeFirmwareFetch,
        subscriber: ArchiveEventSubscriber<FirmwareFetchEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.firmware_fetch.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribePairingEvent> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribePairingEvent,
        subscriber: ArchiveEventSubscriber<PairingEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        if let Some(device) = &self.state.guard().paired_device {
            subscriber
                .send(&PairingEvent::PairingComplete { device_name: device.name.clone(), new: false })
                .ok();
        } else {
            subscriber.send(&PairingEvent::Disconnected).ok();
        }

        self.message_subscribers.pairing_event.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<SubscribeRestoreMagicBackup> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeRestoreMagicBackup,
        subscriber: ArchiveEventSubscriber<foundation_api::backup::RestoreMagicBackupEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.message_subscribers.restore_magic_backup.push(subscriber);
        Ok(())
    }
}

impl server::ScalarEventSubscriptionHandler<SubscribeConnectionStatus> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: SubscribeConnectionStatus,
        subscriber: server::ScalarEventSubscriber<quantum_link::ConnectionStatus>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        let status = self.connection_status();
        if subscriber.send(&status).is_ok() {
            self.message_subscribers.connection_status.push(subscriber);
        }
        Ok(())
    }
}

impl server::BlockingArchiveAsyncHandler<SendAccountUpdate> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<SendAccountUpdate>, _context: &mut ServerContext<Self>) {
        let update = &request.message;
        let ql_message = QuantumLinkMessage::AccountUpdate(foundation_api::bitcoin::AccountUpdate {
            account_id: update.account_id.clone(),
            update: update.update.clone(),
        });
        if self.send(ql_message, SendOutcome::Respond(request.response)).is_ok() {
            self.message_subscribers.published_account_update.send_nowait(&update);
        }
    }

    fn default_response() -> <SendAccountUpdate as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<PublishPsbt> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<PublishPsbt>, _context: &mut ServerContext<Self>) {
        let quantum_link_message = QuantumLinkMessage::BroadcastTransaction(request.message.transaction);
        self.send(quantum_link_message, SendOutcome::Respond(request.response)).ok();
    }

    fn default_response() -> <PublishPsbt as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<EnvoyMagicBackupEnabled> for QuantumLinkServer {
    fn handle(
        &mut self,
        request: ArchiveRequest<EnvoyMagicBackupEnabled>,
        _context: &mut ServerContext<Self>,
    ) {
        match self.send(
            QuantumLinkMessage::EnvoyMagicBackupEnabledRequest(EnvoyMagicBackupEnabledRequest {}),
            SendOutcome::NotifyOnFailure(PendingRequestKind::EnvoyMagicBackupEnabled),
        ) {
            Ok(_) => self.pending.envoy_magic_backup_enabled = Some(PendingRequest::new(request)),
            Err(e) => {
                request.response.respond(Err(e)).ok();
            }
        }
    }

    fn default_response() -> <EnvoyMagicBackupEnabled as server::BlockingArchive>::Response {
        log::error!("failed to respond to magic backup enabled request");
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<CheckFirmwareUpdate> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<CheckFirmwareUpdate>, _context: &mut ServerContext<Self>) {
        let current_version = match self.get_current_os_version() {
            Some(version) => version,
            None => return,
        };

        match self.send(
            QuantumLinkMessage::FirmwareUpdateCheckRequest(FirmwareUpdateCheckRequest { current_version }),
            SendOutcome::NotifyOnFailure(PendingRequestKind::CheckFirmwareUpdate),
        ) {
            Ok(_) => {
                self.pending.update_check = Some(PendingRequest::new(request));
            }
            Err(e) => {
                request.response.respond(Err(e)).ok();
            }
        }
    }

    fn default_response() -> <CheckFirmwareUpdate as server::BlockingArchive>::Response {
        log::error!("failed to respond to firmware check request");
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<StartFirmwareUpdate> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<StartFirmwareUpdate>, _context: &mut ServerContext<Self>) {
        let current_version = match self.get_current_os_version() {
            Some(version) => version,
            None => return,
        };

        let fetch_request =
            FirmwareFetchRequest { current_version, chunk_offset: request.message.chunk_offset };

        if let Err(e) = self.send(
            QuantumLinkMessage::FirmwareFetchRequest(fetch_request),
            SendOutcome::NotifyOnFailure(PendingRequestKind::StartFirmwareUpdate),
        ) {
            let _ = request.response.respond(Err(e)).ok();
            return;
        }

        self.pending.update_start = Some(PendingRequest::new(request));
    }

    fn default_response() -> <StartFirmwareUpdate as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<BackupShard> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<BackupShard>, _context: &mut ServerContext<Self>) {
        let shard = Shard(backup_shard::Shard::encode(&request.message.shard));
        let backup_request = BackupShardRequest { shard };
        let message = QuantumLinkMessage::BackupShardRequest(backup_request);

        if let Err(e) = self.send(message, SendOutcome::NotifyOnFailure(PendingRequestKind::BackupShard)) {
            let _ = request.response.respond(Err(e)).ok();
            return;
        }

        self.pending.backup_shard = Some(PendingRequest::new(request));
    }

    fn default_response() -> <BackupShard as server::BlockingArchive>::Response {
        log::info!("failed to respond to backup shard request");
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<RestoreShard> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<RestoreShard>, _context: &mut ServerContext<Self>) {
        let restore_request = RestoreShardRequest {
            seed_fingerprint: request.message.seed_fingerprint.clone(),
            timestamp: Some(request.message.timestamp),
        };
        let message = QuantumLinkMessage::RestoreShardRequest(restore_request);

        if let Err(e) = self.send(message, SendOutcome::NotifyOnFailure(PendingRequestKind::RestoreShard)) {
            request.response.respond(Err(e)).ok();
            return;
        }

        self.pending.restore_shard = Some(PendingRequest::new(request));
    }

    fn default_response() -> <RestoreShard as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<NotifyOnboardingState> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<NotifyOnboardingState>, _context: &mut ServerContext<Self>) {
        let message = QuantumLinkMessage::OnboardingState(request.message.state);
        self.send(message, SendOutcome::Respond(request.response)).ok();
    }

    fn default_response() -> <NotifyOnboardingState as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<SendApplyPassphrase> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<SendApplyPassphrase>, _context: &mut ServerContext<Self>) {
        let message = QuantumLinkMessage::ApplyPassphrase(foundation_api::bitcoin::ApplyPassphrase {
            fingerprint: request.message.fingerprint,
        });
        self.send(message, SendOutcome::Respond(request.response)).ok();
    }

    fn default_response() -> <SendApplyPassphrase as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<NotifyFirmwareInstall> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<NotifyFirmwareInstall>, _context: &mut ServerContext<Self>) {
        let message = QuantumLinkMessage::FirmwareInstallEvent(request.message.event);
        self.send(message, SendOutcome::Respond(request.response)).ok();
    }

    fn default_response() -> <NotifyFirmwareInstall as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<SendMagicBackupEvent> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<SendMagicBackupEvent>, _context: &mut ServerContext<Self>) {
        let message = QuantumLinkMessage::CreateMagicBackupEvent(request.message.event);
        self.send(message, SendOutcome::Respond(request.response)).ok();
    }

    fn default_response() -> <SendMagicBackupEvent as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<SendRestoreMagicBackupResult> for QuantumLinkServer {
    fn handle(
        &mut self,
        request: ArchiveRequest<SendRestoreMagicBackupResult>,
        _context: &mut ServerContext<Self>,
    ) {
        let message = QuantumLinkMessage::RestoreMagicBackupResult(request.message.result);
        self.send(message, SendOutcome::Respond(request.response)).ok();
    }

    fn default_response() -> <SendRestoreMagicBackupResult as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<AwaitCreateMagicBackupResult> for QuantumLinkServer {
    fn handle(
        &mut self,
        request: ArchiveRequest<AwaitCreateMagicBackupResult>,
        _context: &mut ServerContext<Self>,
    ) {
        self.pending.create_magic_backup_result = Some(PendingRequest::new(request));
    }

    fn default_response() -> <AwaitCreateMagicBackupResult as server::BlockingArchive>::Response {
        log::error!("failed to respond to AwaitCreateMagicBackupResult");
        CreateMagicBackupResult::Error { error: String::from("cancelled") }
    }
}

impl server::BlockingArchiveAsyncHandler<MagicBackupStatus> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<MagicBackupStatus>, _context: &mut ServerContext<Self>) {
        let Some(seed_fingerprint) = self.get_seed_fingerprint() else { return };
        let msg = QuantumLinkMessage::PrimeMagicBackupStatusRequest(PrimeMagicBackupStatusRequest {
            seed_fingerprint: SeedFingerprint(seed_fingerprint),
            timestamp: None,
        });
        match self.send(msg, SendOutcome::NotifyOnFailure(PendingRequestKind::MagicBackupStatus)) {
            Ok(()) => {
                self.pending.prime_magic_backup_status_response = Some(PendingRequest::new(request));
            }
            Err(e) => {
                request.response.respond(Err(e)).ok();
            }
        }
    }

    fn default_response() -> <MagicBackupStatus as server::BlockingArchive>::Response {
        log::error!("failed to respond to MagicBackupStatus");
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<StartRestoreMagicBackup> for QuantumLinkServer {
    fn handle(
        &mut self,
        request: ArchiveRequest<StartRestoreMagicBackup>,
        _context: &mut ServerContext<Self>,
    ) {
        let Some(seed_fingerprint) = self.get_seed_fingerprint() else { return };

        let msg = QuantumLinkMessage::RestoreMagicBackupRequest(RestoreMagicBackupRequest {
            seed_fingerprint: SeedFingerprint(seed_fingerprint),
            resume_from_chunk: 0,
        });

        match self.send(msg, SendOutcome::NotifyOnFailure(PendingRequestKind::RestoreMagicBackup)) {
            Ok(()) => {
                self.pending.restore_magic_backup = Some(PendingRequest::new(request));
            }
            Err(e) => {
                request.response.respond(Err(e)).ok();
            }
        }
    }

    fn default_response() -> <StartRestoreMagicBackup as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<UnpairFromEnvoy> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<UnpairFromEnvoy>, _context: &mut ServerContext<Self>) {
        if self.state.guard().paired_device.is_none() {
            request.response.respond(Ok(())).ok();
            return;
        }

        // only until ble send, no ack required
        self.send(
            QuantumLinkMessage::UnpairingRequest(UnpairingRequest {}),
            SendOutcome::Respond(request.response),
        )
        .ok();

        self.state.guard().paired_device = None;
        self.last_received_name = None;
        self.set_heartbeat_state(|h| *h = HeartbeatState::DEAD);
        self.message_subscribers.pairing_event.send_nowait(&PairingEvent::Disconnected);
    }

    fn default_response() -> <UnpairFromEnvoy as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<SendPrimeMagicBackupEnabled> for QuantumLinkServer {
    fn handle(
        &mut self,
        request: ArchiveRequest<SendPrimeMagicBackupEnabled>,
        _context: &mut ServerContext<Self>,
    ) {
        let Some(seed_fingerprint) = self.get_seed_fingerprint() else { return };
        let message = PrimeMagicBackupEnabled {
            enabled: request.message.enabled,
            seed_fingerprint: SeedFingerprint(seed_fingerprint),
        };
        let message = QuantumLinkMessage::PrimeMagicBackupEnabled(message);
        self.send(message, SendOutcome::Respond(request.response)).ok();
    }

    fn default_response() -> <SendRestoreMagicBackupResult as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::BlockingArchiveAsyncHandler<SendPrimeFiatPreference> for QuantumLinkServer {
    fn handle(
        &mut self,
        request: ArchiveRequest<SendPrimeFiatPreference>,
        _context: &mut ServerContext<Self>,
    ) {
        let message = PrimeFiatPreference { currency_code: request.message.currency_code.clone() };
        let message = QuantumLinkMessage::PrimeFiatPreference(message);
        self.send(message, SendOutcome::Respond(request.response)).ok();
    }

    fn default_response() -> <SendPrimeFiatPreference as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

impl server::ArchiveHandler<BtSendFailure> for QuantumLinkServer {
    fn handle(
        &mut self,
        failure: Owned<BtSendFailure>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        let Ok(failure) = failure.deserialize() else { return };
        let error = SendMessageError::Bluetooth(failure.error);

        match failure.kind {
            PendingRequestKind::EnvoyMagicBackupEnabled => {
                if let Some(req) = self.pending.envoy_magic_backup_enabled.take() {
                    req.respond(Err(error));
                }
            }
            PendingRequestKind::CheckFirmwareUpdate => {
                if let Some(req) = self.pending.update_check.take() {
                    req.respond(Err(error));
                }
            }
            PendingRequestKind::StartFirmwareUpdate => {
                if let Some(req) = self.pending.update_start.take() {
                    req.respond(Err(error));
                }
            }
            PendingRequestKind::BackupShard => {
                if let Some(req) = self.pending.backup_shard.take() {
                    req.respond(Err(error));
                }
            }
            PendingRequestKind::RestoreShard => {
                if let Some(req) = self.pending.restore_shard.take() {
                    req.respond(Err(error));
                }
            }
            PendingRequestKind::RestoreMagicBackup => {
                if let Some(req) = self.pending.restore_magic_backup.take() {
                    req.respond(Err(error));
                }
            }
            PendingRequestKind::MagicBackupStatus => {
                if let Some(req) = self.pending.prime_magic_backup_status_response.take() {
                    req.respond(Err(error));
                }
            }
            PendingRequestKind::EnvoyTimezone => {
                if let Some(req) = self.pending.timezone.take() {
                    req.respond(Err(error));
                }
            }
        }
    }
}

impl server::ScalarEventHandler<bt::State> for QuantumLinkServer {
    fn handle(&mut self, state: bt::State, _sender: PID, _context: &mut ServerContext<Self>) {
        self.set_ble_state(state);
        self.pending.cleanup_expired();
    }
}

impl server::ScalarEventHandler<power_manager::Status> for QuantumLinkServer {
    fn handle(&mut self, state: power_manager::Status, _sender: PID, _context: &mut ServerContext<Self>) {
        self.pwr_state = Some(state);
    }
}

impl server::ScalarEventHandler<fs::FileSystemEvent> for QuantumLinkServer {
    fn handle(
        &mut self,
        msg: fs::FileSystemEvent,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if msg.location == fs::Location::AppData && msg.event_type == fs::FileSystemEventType::Mounted {
            log::info!("AppData mounted, saving state");
            if let Err(e) = self.state.save() {
                log::error!("Error saving state on fs mount: {e:?}");
            }
        }
    }
}

/// originally, state is saved to the system partition. when the encrypted fs partition mounts, we
/// delete the state file from system and write it to the encrypted partition.
///
/// however, if we then restore from backup at the end of onboarding, it overwrites the encrypted
/// partition, deleting our saved state.
///
/// this handler ensures the state is re-written to disk after onboarding (and any backup restore)
/// completes.
impl server::ArchiveEventHandler<settings::global::OnboardingStatus> for QuantumLinkServer {
    fn handle(
        &mut self,
        msg: Owned<settings::global::OnboardingStatus>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        if msg.is_complete() {
            log::info!("onboarding complete, saving state");
            self.state.mark_dirty();
            self.state
                .save()
                .inspect_err(|e| log::warn!("failed to save ql state after onboarding complete {e:?}"))
                .ok();
        }
    }
}

impl server::ArchiveEventHandler<settings::global::DeviceName> for QuantumLinkServer {
    fn handle(
        &mut self,
        _msg: Owned<settings::global::DeviceName>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.publish_device_name();
    }
}

impl server::ScalarEventHandler<EnvoyTimeSync> for QuantumLinkServer {
    fn handle(&mut self, msg: EnvoyTimeSync, _sender: PID, _context: &mut ServerContext<Self>) {
        self.should_set_system_time = msg.0;
    }
}

impl server::ScalarHandler<BtRecvWake> for QuantumLinkServer {
    fn handle(&mut self, _msg: BtRecvWake, _sender: PID, _context: &mut ServerContext<Self>) {
        while let Ok(msg) = self.bt_rx.try_recv() {
            match self.handle_message(&msg) {
                Ok(_) => {}
                Err(e) => {
                    log::error!("Failed to handle dechunked message: {:?}", e);
                }
            }
        }
    }
}

impl server::ScalarHandler<HeartbeatTick> for QuantumLinkServer {
    fn handle(&mut self, _msg: HeartbeatTick, _sender: PID, _context: &mut ServerContext<Self>) {
        self.heartbeat_tick();
        self.pending.cleanup_expired();
    }
}

impl server::ArchiveHandler<HeartbeatSendResult> for QuantumLinkServer {
    fn handle(
        &mut self,
        result: Owned<HeartbeatSendResult>,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        let Ok(result) = result.deserialize() else { return };
        self.handle_heartbeat_send_result(result.result);
    }
}

impl server::BlockingArchiveAsyncHandler<EnvoyTimezone> for QuantumLinkServer {
    fn handle(&mut self, request: ArchiveRequest<EnvoyTimezone>, _context: &mut ServerContext<Self>) {
        let message = QuantumLinkMessage::TimezoneRequest(TimezoneRequest {});

        if let Err(e) = self.send(message, SendOutcome::NotifyOnFailure(PendingRequestKind::EnvoyTimezone)) {
            request.response.respond(Err(e)).ok();
            return;
        }

        self.pending.timezone = Some(PendingRequest::new(request));
    }

    fn default_response() -> <EnvoyTimezone as server::BlockingArchive>::Response {
        Err(SendMessageError::Cancelled)
    }
}

fn log_message(prefix: &str, msg: &QuantumLinkMessage) {
    match msg {
        QuantumLinkMessage::OnboardingState(_) => {
            log::info!("{prefix} {msg:?}");
        }
        QuantumLinkMessage::ExchangeRate(_) => {
            log::info!("{prefix} ExchangeRate");
        }
        QuantumLinkMessage::ExchangeRateHistory(_) => {
            log::info!("{prefix} ExchangeRateHistory");
        }
        QuantumLinkMessage::FirmwareUpdateCheckRequest(_) => {
            log::info!("{prefix} FirmwareUpdateCheckRequest");
        }
        QuantumLinkMessage::FirmwareUpdateCheckResponse(_) => {
            log::info!("{prefix} FirmwareUpdateCheckResponse");
        }
        QuantumLinkMessage::FirmwareFetchRequest(_) => {
            log::info!("{prefix} FirmwareFetchRequest");
        }
        QuantumLinkMessage::FirmwareFetchEvent(_) => {
            log::info!("{prefix} FirmwareFetchEvent");
        }
        QuantumLinkMessage::FirmwareInstallEvent(_) => {
            log::info!("{prefix} FirmwareInstallEvent");
        }
        QuantumLinkMessage::DeviceStatus(_) => {
            log::info!("{prefix} DeviceStatus");
        }
        QuantumLinkMessage::DeviceNameUpdate(_) => {
            log::info!("{prefix} DeviceNameUpdate");
        }
        QuantumLinkMessage::EnvoyStatus(_) => {
            log::info!("{prefix} EnvoyStatus");
        }
        QuantumLinkMessage::PairingRequest(_) => {
            log::info!("{prefix} PairingRequest");
        }
        QuantumLinkMessage::PairingResponse(_) => {
            log::info!("{prefix} PairingResponse");
        }
        QuantumLinkMessage::SignPsbt(_) => {
            log::info!("{prefix} SignPsbt");
        }
        QuantumLinkMessage::BroadcastTransaction(_) => {
            log::info!("{prefix} BroadcastTransaction");
        }
        QuantumLinkMessage::AccountUpdate(_) => {
            log::info!("{prefix} AccountUpdate");
        }
        QuantumLinkMessage::SecurityCheck(_) => {
            log::info!("{prefix} SecurityChallengeRequest");
        }
        QuantumLinkMessage::EnvoyMagicBackupEnabledRequest(_) => {
            log::info!("{prefix} MagicBackupEnabledRequest");
        }
        QuantumLinkMessage::EnvoyMagicBackupEnabledResponse(_) => {
            log::info!("{prefix} MagicBackupEnabledResponse");
        }
        QuantumLinkMessage::BackupShardRequest(_) => {
            log::info!("{prefix} BackupShardRequest");
        }
        QuantumLinkMessage::BackupShardResponse(_) => {
            log::info!("{prefix} BackupShardResponse");
        }
        QuantumLinkMessage::RestoreShardRequest(_) => {
            log::info!("{prefix} RestoreShardRequest");
        }
        QuantumLinkMessage::RestoreShardResponse(_) => {
            log::info!("{prefix} RestoreShardResponse");
        }
        QuantumLinkMessage::CreateMagicBackupEvent(_) => {
            log::info!("{prefix} CreateMagicBackupEvent")
        }
        QuantumLinkMessage::CreateMagicBackupResult(_) => {
            log::info!("{prefix} CreateMagicBackupResult")
        }
        QuantumLinkMessage::RestoreMagicBackupRequest(_) => {
            log::info!("{prefix} RestoreMagicBackupRequest")
        }
        QuantumLinkMessage::RestoreMagicBackupEvent(_) => {
            log::info!("{prefix} RestoreMagicBackupEvent")
        }
        QuantumLinkMessage::RestoreMagicBackupResult(_) => {
            log::info!("{prefix} RestoreMagicBackupResult")
        }
        QuantumLinkMessage::ApplyPassphrase(_) => {
            log::info!("{prefix} ApplyPassphrase")
        }
        QuantumLinkMessage::PrimeMagicBackupEnabled(_) => {
            log::info!("{prefix} PrimeMagicBackupEnabled")
        }
        QuantumLinkMessage::PrimeMagicBackupStatusRequest(_) => {
            log::info!("{prefix} PrimeMagicBackupStatusRequest")
        }
        QuantumLinkMessage::PrimeMagicBackupStatusResponse(_) => {
            log::info!("{prefix} PrimeMagicBackupStatusResponse")
        }
        QuantumLinkMessage::Heartbeat(_) => {
            log::debug!("{prefix} Heartbeat")
        }
        QuantumLinkMessage::TimezoneRequest(_) => {
            log::info!("{prefix} TimezoneRequest")
        }
        QuantumLinkMessage::TimezoneResponse(_) => {
            log::info!("{prefix} TimezoneResponse")
        }
        QuantumLinkMessage::UnpairingRequest(_) => {
            log::info!("{prefix} UnpairingRequest")
        }
        QuantumLinkMessage::UnpairingResponse(_) => {
            log::info!("{prefix} UnpairingResponse")
        }
        QuantumLinkMessage::MagicBackupRequestV2(_) => {
            log::info!("{prefix} MagicBackupRequestV2")
        }
        QuantumLinkMessage::MagicBackupResponseV2(_) => {
            log::info!("{prefix} MagicBackupResponseV2")
        }
        QuantumLinkMessage::PrimeFiatPreference(_) => {
            log::info!("{prefix} PrimeFiatPreference")
        }
    }
}
