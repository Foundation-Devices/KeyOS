// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-side `rusb` transport. Gated behind the `client` feature so device
//! builds of this crate don't pull in `rusb`/`anyhow`.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rusb::{DeviceHandle, GlobalContext};

use crate::{Command, FrameType, ProtocolError, RawResponse, Status};

/// Passport Prime VID:PID in normal mode.
pub const PASSPORT_VID: u16 = 0x1307;
pub const PASSPORT_PID: u16 = 0x0165;

/// Legacy VID:PID used while a Flux app overrides the USB identity.
pub const LEGACY_VID: u16 = 0x2c97;
pub const LEGACY_PID: u16 = 0x0007;

/// Minimum size of the reader thread's USB IN buffer. Logs arrive unannounced
/// and the device-side log buffer is 16 KiB per chunk, so the floor must
/// comfortably exceed that. Screenshots bump the buffer further via the
/// resize-hint channel.
const READ_BUFFER_FLOOR: usize = 64 * 1024;

pub struct UsbDebugClient {
    handle: Arc<DeviceHandle<GlobalContext>>,
    ep_out: u8,
    log_rx: Receiver<Vec<u8>>,
    resp_rx: Receiver<RawResponse>,
    resize_tx: Sender<usize>,
}

impl UsbDebugClient {
    /// Try `PASSPORT_VID:PID`, then fall back to `LEGACY_VID:PID`. If both fail,
    /// returns the error from the primary attempt (legacy is the rarer case).
    pub fn open() -> Result<Self> {
        match Self::open_with_vid_pid(PASSPORT_VID, PASSPORT_PID) {
            Ok(client) => Ok(client),
            Err(primary_err) => Self::open_with_vid_pid(LEGACY_VID, LEGACY_PID).map_err(|_| primary_err),
        }
    }

    /// Open a specific VID:PID with no fallback.
    pub fn open_with_vid_pid(vid: u16, pid: u16) -> Result<Self> {
        // `mut` is required on some rusb versions (`detach_kernel_driver` takes &mut self)
        // and optional on others; keep it and silence the unused-mut warning.
        #[allow(unused_mut)]
        let mut handle = rusb::open_device_with_vid_pid(vid, pid)
            .ok_or_else(|| anyhow::anyhow!("No USB device found with VID:PID {vid:04x}:{pid:04x}"))?;

        let device = handle.device();
        let config = device.active_config_descriptor().context("reading USB config descriptor")?;

        // Find the vendor-specific interface (class 0xFF) with bulk IN and OUT endpoints.
        let mut debug_iface = None;
        let mut ep_out = None;
        let mut ep_in = None;

        for iface in config.interfaces() {
            for desc in iface.descriptors() {
                if desc.class_code() == 0xFF {
                    for ep in desc.endpoint_descriptors() {
                        if ep.transfer_type() == rusb::TransferType::Bulk {
                            if ep.direction() == rusb::Direction::Out {
                                ep_out = Some(ep.address());
                            } else {
                                ep_in = Some(ep.address());
                            }
                        }
                    }
                    if ep_out.is_some() && ep_in.is_some() {
                        debug_iface = Some(desc.interface_number());
                        break;
                    }
                }
            }
            if debug_iface.is_some() {
                break;
            }
        }

        let debug_iface = debug_iface.context("Vendor debug interface (class 0xFF) not found")?;
        let ep_out = ep_out.context("Debug bulk OUT endpoint not found")?;
        let ep_in = ep_in.context("Debug bulk IN endpoint not found")?;

        // Detach any kernel driver from the debug interface before claiming.
        // Some Linux setups report `kernel_driver_active()` unreliably across
        // the Legacy HID identity transition, so detach on a best-effort basis.
        let _ = handle.detach_kernel_driver(debug_iface);
        handle.claim_interface(debug_iface).context("claiming debug interface")?;

        let handle = Arc::new(handle);
        let (log_tx, log_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();
        let (resize_tx, resize_rx) = mpsc::channel();

        let reader_handle = handle.clone();
        std::thread::spawn(move || reader_thread(reader_handle, ep_in, log_tx, resp_tx, resize_rx));

        Ok(Self { handle, ep_out, log_rx, resp_rx, resize_tx })
    }

    /// Encode `cmd`, send it on the OUT endpoint, and wait up to `timeout` for
    /// the matching `[STATUS][PAYLOAD]` response frame. Validates the status
    /// byte and returns just the payload on success. Internally hints the
    /// reader thread to size its buffer for `cmd.max_response_size()`.
    pub fn send(&self, cmd: Command, timeout: Duration) -> Result<Vec<u8>> {
        let _ = self.resize_tx.send(cmd.max_response_size());

        let mut out_buf = Vec::with_capacity(64);
        cmd.encode_into(&mut out_buf);
        let cmd_byte = cmd.cmd_byte();

        self.handle.write_bulk(self.ep_out, &out_buf, timeout).context("bulk OUT write")?;

        let resp = self
            .resp_rx
            .recv_timeout(timeout)
            .map_err(|_| anyhow::anyhow!("Timeout waiting for response to cmd 0x{cmd_byte:02x}"))?;

        match Status::from_byte(resp.status) {
            Ok(Status::Ok) => Ok(resp.payload),
            Ok(Status::Err) => Err(ProtocolError::DeviceError(resp.status).into()),
            Err(e) => Err(e.into()),
        }
    }

    /// Block up to `timeout` for one log frame. Pass `Duration::ZERO` for a
    /// non-blocking poll. `Disconnected` means the reader thread exited (USB
    /// failure).
    pub fn read_logs(&self, timeout: Duration) -> Result<Vec<u8>, RecvTimeoutError> {
        self.log_rx.recv_timeout(timeout)
    }
}

// DeviceHandle's Drop calls libusb_close which releases all claimed interfaces.
// The reader thread's Arc clone keeps the handle alive until it exits, which
// it does when the receivers are dropped and `send` errors out.
fn reader_thread(
    handle: Arc<DeviceHandle<GlobalContext>>,
    ep_in: u8,
    log_tx: Sender<Vec<u8>>,
    resp_tx: Sender<RawResponse>,
    resize_rx: Receiver<usize>,
) {
    let mut buf = vec![0u8; READ_BUFFER_FLOOR];

    loop {
        // Drain any pending size hints before the next read; grow but never shrink.
        let mut hint = 0;
        while let Ok(size) = resize_rx.try_recv() {
            hint = hint.max(size);
        }
        if hint > buf.len() {
            buf.resize(hint, 0);
        }

        match handle.read_bulk(ep_in, &mut buf, Duration::from_secs(5)) {
            Ok(0) => continue,
            Ok(n) => {
                let frame_byte = buf[0];
                let payload = &buf[1..n];
                match FrameType::from_byte(frame_byte) {
                    Ok(FrameType::Log) => {
                        if log_tx.send(payload.to_vec()).is_err() {
                            return;
                        }
                    }
                    Ok(FrameType::Response) => {
                        if let Some((&status, rest)) = payload.split_first() {
                            let resp = RawResponse { status, payload: rest.to_vec() };
                            if resp_tx.send(resp).is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => {} // unknown frame type -- drop silently
                }
            }
            Err(rusb::Error::Timeout) => continue,
            Err(_) => return,
        }
    }
}
