// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{sync::mpsc, time::Duration};

use bt::BluetoothError;
use btp::chunk;
use quantum_link::SendMessageError;
use server::CheckedConn;

use crate::{
    backoff::ExponentialBackoff, pending_requests::PendingRequestKind, BluetoothApi, InternalPermissions,
};

const BACKOFF: ExponentialBackoff =
    ExponentialBackoff::new(Duration::from_millis(5), Duration::from_millis(1000), 10);

pub struct BtSend {
    pub payload: Vec<u8>,
    pub outcome: SendOutcome,
}

pub enum SendOutcome {
    /// respond to request
    Respond(server::ArchiveResponse<Result<(), SendMessageError>>),

    /// on failure only, notify main server with this kind
    NotifyOnFailure(PendingRequestKind),

    NotifyHeartbeat,

    /// fire-and-forget
    Ignore,
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, server::Message)]
pub struct BtSendFailure {
    pub kind: PendingRequestKind,
    pub error: BluetoothError,
}

#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, server::Message)]
pub struct HeartbeatSendResult {
    pub result: Result<(), BluetoothError>,
}

#[derive(Clone, Default)]
pub struct QuantumLinkSender {
    pub conn: CheckedConn<InternalPermissions>,
}

impl QuantumLinkSender {
    pub fn send(&self, failure: BtSendFailure) { self.conn.try_send_archive(failure).ok(); }

    pub fn send_heartbeat_result(&self, result: HeartbeatSendResult) {
        self.conn.try_send_archive(result).ok();
    }
}

pub fn start_ble_send_thread() -> mpsc::Sender<BtSend> {
    let notify = QuantumLinkSender::default();
    let (mpsc_tx, mpsc_rx) = mpsc::channel::<BtSend>();

    std::thread::spawn(move || {
        xous::set_thread_priority(xous::ThreadPriority::System5).unwrap();
        let mut bt = BluetoothApi::default();

        while let Ok(BtSend { payload, outcome }) = mpsc_rx.recv() {
            log::trace!("Sending {} bytes", payload.len());

            match send_payload(&mut bt, &payload) {
                Ok(()) => match outcome {
                    SendOutcome::Respond(response) => {
                        response.respond(Ok(())).ok();
                    }
                    SendOutcome::NotifyOnFailure(_) => {}
                    SendOutcome::NotifyHeartbeat => {
                        notify.send_heartbeat_result(HeartbeatSendResult { result: Ok(()) });
                    }
                    SendOutcome::Ignore => {}
                },
                Err(bt_error) => {
                    log::error!("BT send failed: {:?}, draining pending sends", bt_error);

                    fail_send(outcome, &bt_error, &notify);

                    // drain and fail all pending sends with the same error
                    while let Ok(pending) = mpsc_rx.try_recv() {
                        fail_send(pending.outcome, &bt_error, &notify);
                    }
                }
            }
        }

        log::info!("Sender exited on ble_send_thread");
    });
    mpsc_tx
}

fn send_payload(bt: &mut BluetoothApi, payload: &[u8]) -> Result<(), BluetoothError> {
    for chunk in chunk(payload) {
        send_chunk(bt, &chunk)?;
    }
    Ok(())
}

fn send_chunk(bt: &mut BluetoothApi, chunk: &[u8]) -> Result<(), BluetoothError> {
    let mut retry = BACKOFF;

    loop {
        match bt.send_ble(chunk) {
            Ok(()) => return Ok(()),
            Err(BluetoothError::BlePacketRejected) => match retry.next() {
                Some(delay) => {
                    log::trace!("BLE packet rejected, retrying after {:?}", delay);
                    std::thread::sleep(delay);
                }
                None => {
                    log::error!("BLE packet rejected, no more retries");
                    return Err(BluetoothError::BlePacketRejected);
                }
            },
            Err(e) => {
                log::error!("Failed to send BLE packet: {:?}", e);
                return Err(e);
            }
        }
    }
}

fn fail_send(outcome: SendOutcome, error: &BluetoothError, notify: &QuantumLinkSender) {
    match outcome {
        SendOutcome::Respond(response) => {
            response.respond(Err(SendMessageError::Bluetooth(*error))).ok();
        }
        SendOutcome::NotifyOnFailure(kind) => {
            notify.send(BtSendFailure { kind, error: *error });
        }
        SendOutcome::NotifyHeartbeat => {
            notify.send_heartbeat_result(HeartbeatSendResult { result: Err(*error) });
        }
        SendOutcome::Ignore => {
            log::warn!("Fire-and-forget send failed: {:?}", error);
        }
    }
}
