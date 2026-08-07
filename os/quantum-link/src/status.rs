// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use foundation_api::message::QuantumLinkMessage;
use server::MessageId as _;

use crate::{bt_tx_bridge::SendOutcome, QuantumLinkServer};

const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6);
const HEARTBEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const MAX_MISSED_HEARTBEATS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatState {
    pub live: bool,
    pub request: RequestState,
    pub time_sync: Option<[u8; 16]>,
}

impl HeartbeatState {
    pub const DEAD: HeartbeatState =
        HeartbeatState { live: false, request: RequestState::Idle, time_sync: None };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestState {
    /// Not doing anything, waiting for next heartbeat interval
    Idle,
    /// Heartbeat queued, waiting for BT send confirmation
    PendingSend,
    /// Heartbeat sent over BT, waiting for reply
    WaitingForResponse,
}

#[derive(Debug, Clone, Copy, server::Message)]
pub struct HeartbeatTick;

impl QuantumLinkServer {
    pub fn set_ble_state(&mut self, state: bt::State) {
        let was_connected = self.bt_state.is_connected();
        let is_connected = state.is_connected();

        self.bt_state = state;

        if !was_connected && is_connected {
            self.missed_heartbeats = 0;
            self.heartbeat_tick();
            self.publish_device_name();
        }

        if was_connected && !is_connected {
            self.heartbeat_state = HeartbeatState::DEAD;
            self.missed_heartbeats = 0;
            // Heartbeat ticks stop while disconnected, so the periodic timeout
            // path won't fire. Fail any in-flight requests now.
            self.pending.expire_all_now();
        }

        self.notify_connection_status();
    }

    pub fn set_heartbeat_state(&mut self, f: impl FnOnce(&mut HeartbeatState)) {
        f(&mut self.heartbeat_state);
        self.notify_connection_status();
    }

    pub fn notify_connection_status(&mut self) {
        let status = self.connection_status();
        if status != self.last_status {
            self.last_status = status;
            self.message_subscribers.connection_status.send_nowait(&status);
        }
    }

    pub fn connection_status(&mut self) -> quantum_link::ConnectionStatus {
        quantum_link::ConnectionStatus {
            bt_connected: self.bt_state.is_connected(),
            ql_paired: self.state.guard().paired_device.is_some(),
            live: self.heartbeat_state.live,
        }
    }

    pub fn heartbeat_tick(&mut self) {
        match self.heartbeat_state.request {
            RequestState::Idle => {
                if !self.bt_state.is_connected() || self.state.guard().paired_device.is_none() {
                    return;
                }
                log::debug!("sending heartbeat from idle state");
                let mut request_id = [0; 16];
                getrandom::getrandom(&mut request_id).unwrap();
                let res = self.send(
                    QuantumLinkMessage::Heartbeat(foundation_api::status::Heartbeat {
                        request_id: Some(request_id),
                        timestamp_ms: None,
                    }),
                    SendOutcome::NotifyHeartbeat,
                );
                match res {
                    Ok(_) => {
                        self.heartbeat_state.time_sync = Some(request_id);
                        self.set_heartbeat_state(|h| h.request = RequestState::PendingSend);
                    }
                    Err(e) => {
                        log::debug!("failed to send heartbeat: {e:?}");
                        self.heartbeat_failure();
                        if self.bt_state.is_connected() {
                            self.reschedule_heartbeat(HEARTBEAT_INTERVAL);
                        }
                    }
                }
            }
            RequestState::PendingSend => {
                log::warn!("heartbeat tick when pending send...");
            }
            RequestState::WaitingForResponse => {
                log::debug!("heartbeat timeout");
                self.heartbeat_state.time_sync = None;
                self.heartbeat_failure();
                self.reschedule_heartbeat(HEARTBEAT_INTERVAL);
            }
        }
    }

    pub fn handle_heartbeat_send_result(&mut self, result: Result<(), bt::BluetoothError>) {
        match result {
            Ok(()) => {
                log::debug!("heartbeat sent, waiting for ack");
                self.set_heartbeat_state(|h| h.request = RequestState::WaitingForResponse);
                self.reschedule_heartbeat(HEARTBEAT_TIMEOUT);
            }
            Err(e) => {
                log::debug!("failed to send heartbeat over BT: {e:?}");
                self.heartbeat_failure();
                self.reschedule_heartbeat(HEARTBEAT_INTERVAL);
            }
        }
    }

    fn heartbeat_failure(&mut self) {
        self.missed_heartbeats += 1;
        if self.missed_heartbeats >= MAX_MISSED_HEARTBEATS {
            log::warn!("connection is dead after {} missed heartbeats", self.missed_heartbeats);
            self.set_heartbeat_state(|h| *h = HeartbeatState::DEAD);
        } else {
            log::debug!("missed heartbeat ({}/{})", self.missed_heartbeats, MAX_MISSED_HEARTBEATS);
            self.set_heartbeat_state(|h| h.request = RequestState::Idle);
        }
    }

    pub fn heartbeat_success(&mut self) {
        self.heartbeat_state.time_sync = None;
        self.missed_heartbeats = 0;
        self.heartbeat_state.live = true;
        self.set_heartbeat_state(|h| h.request = RequestState::Idle);
        self.reschedule_heartbeat(HEARTBEAT_INTERVAL);
    }

    fn reschedule_heartbeat(&mut self, duration: std::time::Duration) {
        self.heartbeat_cb.request(duration.as_millis() as usize, HeartbeatTick::ID, 0);
    }
}
