// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-side `rusb` transport. Gated behind the `client` feature so device
//! builds of this crate don't pull in `rusb`/`anyhow`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use rusb::{DeviceHandle, GlobalContext};

use crate::{Command, FrameType, ProtocolError, RawResponse, Status, USB_DEBUG_BULK_MAX_PACKET_LEN};

/// Passport Prime VID:PID in normal mode.
pub const PASSPORT_VID: u16 = 0x1307;
pub const PASSPORT_PID: u16 = 0x0165;

/// Legacy VID:PID used while a Flux app overrides the USB identity.
pub const LEGACY_VID: u16 = 0x2c97;
pub const LEGACY_PID: u16 = 0x0007;

/// USB read chunk size. Protocol frames may be larger than this; the reader
/// accumulates chunks until a short packet or ZLP terminates the frame.
const READ_CHUNK_LEN: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_IN_FRAME_LEN: usize = 2 * 1024 * 1024;

pub struct UsbDebugClient {
    handle: Arc<DeviceHandle<GlobalContext>>,
    ep_out: u8,
    log_rx: Receiver<Vec<u8>>,
    resp_rx: Receiver<RawResponse>,
    reader_enabled: Arc<AtomicBool>,
    reader_thread: Option<JoinHandle<()>>,
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
        let reader_enabled = Arc::new(AtomicBool::new(true));

        let reader_handle = handle.clone();
        let reader_gate = reader_enabled.clone();
        let reader_thread =
            std::thread::spawn(move || reader_thread(reader_handle, ep_in, log_tx, resp_tx, reader_gate));

        Ok(Self { handle, ep_out, log_rx, resp_rx, reader_enabled, reader_thread: Some(reader_thread) })
    }

    /// Encode `cmd`, send it on the OUT endpoint, and wait up to `timeout` for
    /// the matching `[STATUS][PAYLOAD]` response frame. Validates the status
    /// byte and returns just the payload on success.
    pub fn send(&self, cmd: Command, timeout: Duration) -> Result<Vec<u8>> {
        let mut out_buf = Vec::with_capacity(64);
        cmd.encode_into(&mut out_buf);
        let cmd_byte = cmd.cmd_byte();

        self.handle.write_bulk(self.ep_out, &out_buf, timeout).context("bulk OUT write")?;
        if needs_out_zlp(out_buf.len()) {
            self.handle.write_bulk(self.ep_out, &[], timeout).context("bulk OUT ZLP write")?;
        }

        let resp = self
            .resp_rx
            .recv_timeout(timeout)
            .map_err(|_| anyhow::anyhow!("Timeout waiting for response to cmd 0x{cmd_byte:02x}"))?;

        match Status::from_byte(resp.status) {
            Ok(Status::Ok) => Ok(resp.payload),
            Ok(Status::Err) => Err(ProtocolError::DeviceError(resp.status).into()),
            Ok(Status::Locked) => Err(ProtocolError::DeviceLocked.into()),
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

fn needs_out_zlp(len: usize) -> bool { len > 0 && len % USB_DEBUG_BULK_MAX_PACKET_LEN == 0 }

impl Drop for UsbDebugClient {
    fn drop(&mut self) {
        self.reader_enabled.store(false, Ordering::Release);
        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
    }
}

// DeviceHandle's Drop calls libusb_close, which releases all claimed
// interfaces. Gate the reader off on client drop so its Arc clone is released
// before `disconnect` returns and another process can claim the interface.
fn reader_thread(
    handle: Arc<DeviceHandle<GlobalContext>>,
    ep_in: u8,
    log_tx: Sender<Vec<u8>>,
    resp_tx: Sender<RawResponse>,
    reader_enabled: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; READ_CHUNK_LEN];
    let mut pending = Vec::with_capacity(READ_CHUNK_LEN);

    while reader_enabled.load(Ordering::Acquire) {
        match handle.read_bulk(ep_in, &mut buf, READ_TIMEOUT) {
            Ok(0) => {
                if !finish_pending_frame(&mut pending, &log_tx, &resp_tx) {
                    return;
                }
            }
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                if pending.len() > MAX_IN_FRAME_LEN {
                    pending.clear();
                    continue;
                }
                if n < READ_CHUNK_LEN && !finish_pending_frame(&mut pending, &log_tx, &resp_tx) {
                    return;
                }
            }
            Err(rusb::Error::Timeout) => continue,
            Err(_) => return,
        }
    }
}

fn finish_pending_frame(
    pending: &mut Vec<u8>,
    log_tx: &Sender<Vec<u8>>,
    resp_tx: &Sender<RawResponse>,
) -> bool {
    if pending.is_empty() {
        return true;
    }

    let frame_byte = pending[0];
    let payload = &pending[1..];
    let keep_reading = match FrameType::from_byte(frame_byte) {
        Ok(FrameType::Log) => log_tx.send(payload.to_vec()).is_ok(),
        Ok(FrameType::Response) => {
            if let Some((&status, rest)) = payload.split_first() {
                resp_tx.send(RawResponse { status, payload: rest.to_vec() }).is_ok()
            } else {
                true
            }
        }
        Err(_) => true, // unknown frame type -- drop silently
    };
    pending.clear();
    keep_reading
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(frame_type: FrameType, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + payload.len());
        out.push(frame_type as u8);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn assembles_response_until_short_packet() {
        let (log_tx, log_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();
        let mut pending = Vec::new();
        let mut response = frame(FrameType::Response, &[Status::Ok as u8]);
        response.resize(READ_CHUNK_LEN + 4, 0xaa);
        response[1] = Status::Ok as u8;

        pending.extend_from_slice(&response[..READ_CHUNK_LEN]);
        assert!(resp_rx.try_recv().is_err());
        assert!(log_rx.try_recv().is_err());

        pending.extend_from_slice(&response[READ_CHUNK_LEN..]);
        assert!(finish_pending_frame(&mut pending, &log_tx, &resp_tx));

        let resp = resp_rx.try_recv().expect("response");
        assert_eq!(resp.status, Status::Ok as u8);
        assert_eq!(resp.payload, vec![0xaa; READ_CHUNK_LEN + 2]);
        assert!(pending.is_empty());
    }

    #[test]
    fn zlp_terminates_max_packet_aligned_frame() {
        let (log_tx, log_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();
        let mut pending = Vec::new();
        pending.extend_from_slice(&frame(FrameType::Log, &[0xaa; READ_CHUNK_LEN - 1]));

        assert!(finish_pending_frame(&mut pending, &log_tx, &resp_tx));

        assert_eq!(log_rx.try_recv().expect("log"), vec![0xaa; READ_CHUNK_LEN - 1]);
        assert!(resp_rx.try_recv().is_err());
        assert!(pending.is_empty());
    }

    #[test]
    fn max_packet_aligned_out_writes_need_zlp() {
        assert!(!needs_out_zlp(0));
        assert!(!needs_out_zlp(USB_DEBUG_BULK_MAX_PACKET_LEN - 1));
        assert!(needs_out_zlp(USB_DEBUG_BULK_MAX_PACKET_LEN));
        assert!(needs_out_zlp(USB_DEBUG_BULK_MAX_PACKET_LEN * 2));
        assert!(!needs_out_zlp(USB_DEBUG_BULK_MAX_PACKET_LEN * 2 + 1));
    }
}
