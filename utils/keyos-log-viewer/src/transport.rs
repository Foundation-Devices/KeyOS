// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! USB-debug transport thread.
//!
//! Owns a `UsbDebugClient`, reconnects on failure, forwards log records to
//! the UI and processes outbound commands from the UI.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use usb_debug_protocol::{Command, UsbDebugClient};

use crate::AppEvent;

const LOG_RECORD_TERMINATOR: u8 = 0x1e;

#[derive(Debug)]
pub enum TransportEvent {
    Payload(String),
    Status(String),
}

#[derive(Debug, Clone, Copy)]
pub enum TransportCommand {
    RequestProcessList,
}

pub fn start_transport_thread(
    reconnect_timeout: Duration,
    sender: Sender<AppEvent>,
    command_receiver: Receiver<TransportCommand>,
) {
    thread::spawn(move || {
        let mut state = TransportState {
            reconnect_timeout,
            sender,
            command_receiver,
            log_buf: Vec::new(),
            last_status: String::new(),
        };
        state.run();
    });
}

struct TransportState {
    reconnect_timeout: Duration,
    sender: Sender<AppEvent>,
    command_receiver: Receiver<TransportCommand>,
    /// Raw byte buffer for accumulating log data across frames.
    log_buf: Vec<u8>,
    last_status: String,
}

#[derive(Debug)]
enum TransportThreadError {
    ChannelClosed,
    UsbFailure,
}

impl TransportState {
    fn run(&mut self) { let _ = self.run_inner(); }

    fn run_inner(&mut self) -> Result<(), TransportThreadError> {
        self.send_status("Initializing...")?;

        loop {
            let client = self.connect_with_retry()?;
            self.log_buf.clear();

            match self.read_loop(&client) {
                TransportThreadError::ChannelClosed => return Err(TransportThreadError::ChannelClosed),
                TransportThreadError::UsbFailure => {
                    self.send_status("USB failure, reconnecting...")?;
                }
            }
        }
    }

    fn connect_with_retry(&mut self) -> Result<UsbDebugClient, TransportThreadError> {
        let retry_interval_secs = self.reconnect_timeout.as_secs().max(1);
        loop {
            match UsbDebugClient::open() {
                Ok(client) => {
                    self.send_status("Connected via USB vendor interface")?;
                    return Ok(client);
                }
                Err(err) => {
                    let reason = format!("{err}");
                    for seconds_left in (1..=retry_interval_secs).rev() {
                        self.send_status(&format!("{reason} - retrying in {seconds_left}s"))?;
                        thread::sleep(Duration::from_secs(1));
                    }
                }
            }
        }
    }

    fn read_loop(&mut self, client: &UsbDebugClient) -> TransportThreadError {
        loop {
            if let Err(e) = self.process_commands(client) {
                return e;
            }

            match client.read_logs(Duration::from_millis(100)) {
                Ok(data) => {
                    self.log_buf.extend_from_slice(&data);
                    if let Err(e) = self.flush_log_records() {
                        return e;
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return TransportThreadError::UsbFailure,
            }
        }
    }

    fn process_commands(&mut self, client: &UsbDebugClient) -> Result<(), TransportThreadError> {
        loop {
            match self.command_receiver.try_recv() {
                Ok(TransportCommand::RequestProcessList) => match client
                    .send(Command::KernelCmd { cmd_byte: b't' }, Duration::from_secs(5))
                {
                    Ok(payload) => {
                        let output = String::from_utf8_lossy(&payload).to_string();
                        if !output.trim().is_empty() {
                            self.send_payload(output)?;
                        }
                    }
                    Err(err) => {
                        self.send_status(&format!("Process-list request failed: {err}"))?;
                        return Err(TransportThreadError::UsbFailure);
                    }
                },
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => return Err(TransportThreadError::ChannelClosed),
            }
        }
    }

    /// Extract 0x1E-terminated log records from `log_buf`.
    fn flush_log_records(&mut self) -> Result<(), TransportThreadError> {
        while let Some(pos) = self.log_buf.iter().position(|b| *b == LOG_RECORD_TERMINATOR) {
            let payload =
                String::from_utf8_lossy(&self.log_buf[..pos]).trim_end_matches(['\r', '\n']).to_string();
            self.log_buf.drain(..=pos);

            if payload.is_empty() {
                continue;
            }

            self.send_payload(payload)?;
        }
        Ok(())
    }

    fn send_payload(&self, payload: String) -> Result<(), TransportThreadError> {
        self.sender
            .send(AppEvent::Transport(TransportEvent::Payload(payload)))
            .map_err(|_| TransportThreadError::ChannelClosed)
    }

    fn send_status(&mut self, status: &str) -> Result<(), TransportThreadError> {
        if self.last_status == status {
            return Ok(());
        }
        self.last_status = status.to_string();
        self.sender
            .send(AppEvent::Transport(TransportEvent::Status(status.to_string())))
            .map_err(|_| TransportThreadError::ChannelClosed)
    }
}
