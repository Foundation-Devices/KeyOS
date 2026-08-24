// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-side client. Gated behind the `client` feature so device builds of
//! this crate don't pull in `rusb`.
//!
//! One [`DebugClient`] speaks to a device over USB bulk endpoints or to a
//! hosted simulator over its loopback socket. The transports differ only in
//! how byte frames travel, so that is the specialization boundary: a sink and
//! a source per transport, everything above them written once.

use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusb::{DeviceHandle, GlobalContext};
use thiserror::Error;

use crate::{
    stream, Command, FrameType, ProtocolError, RawResponse, Status, MAX_FRAME_LEN,
    USB_DEBUG_BULK_MAX_PACKET_LEN,
};

/// Why opening the debug channel failed.
#[derive(Debug, Error)]
pub enum OpenError {
    #[error("no USB device found with VID:PID {vid:04x}:{pid:04x}")]
    NotFound { vid: u16, pid: u16 },
    #[error("reading USB config descriptor: {0}")]
    ConfigDescriptor(#[source] rusb::Error),
    #[error("no vendor debug interface (class 0xFF) with both bulk endpoints")]
    NoDebugInterface,
    #[error("claiming debug interface: {0}")]
    ClaimInterface(#[source] rusb::Error),
    #[error("connecting to the simulator debug channel at {addr}: {source}")]
    SimConnect {
        addr: String,
        #[source]
        source: std::io::Error,
    },
}

/// Why a command exchange broke down. The device may still be part-way through reading a frame, so
/// a connection that reports this cannot be reused.
#[derive(Debug, Error)]
pub enum OutOfSync {
    #[error("bulk OUT write: {0}")]
    Write(#[source] rusb::Error),
    #[error("bulk OUT write made no progress after {written} of {total} bytes")]
    NoProgress { written: usize, total: usize },
    #[error("bulk OUT short write ended the frame after {written} of {total} bytes")]
    ShortWrite { written: usize, total: usize },
    #[error("bulk OUT write reported {count} bytes for a {remaining}-byte remainder")]
    OverRun { count: usize, remaining: usize },
    #[error("{0} timed out")]
    TimedOut(&'static str),
    #[error("no response to cmd 0x{cmd:02x}")]
    NoResponse { cmd: u8 },
    #[error("simulator debug channel write: {0}")]
    StreamWrite(#[source] std::io::Error),
    #[error("the debug channel closed; the device disconnected or another client is driving it")]
    Closed,
}

/// A failed `DebugClient` command.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error(transparent)]
    OutOfSync(#[from] OutOfSync),
    /// The device answered and refused. The transport stays usable.
    #[error("device returned status 0x{0:02x}")]
    Device(u8),
    /// Refused because the lock screen is active. The transport stays usable.
    #[error("device is locked")]
    Locked,
    #[error(transparent)]
    Response(#[from] ProtocolError),
}

/// Passport Prime VID:PID in normal mode.
pub const PASSPORT_VID: u16 = 0x1307;
pub const PASSPORT_PID: u16 = 0x0165;

/// Legacy VID:PID used while a Flux app overrides the USB identity.
pub const LEGACY_VID: u16 = 0x2c97;
pub const LEGACY_PID: u16 = 0x7011;

/// USB read chunk size. Protocol frames may be larger than this; the reader
/// accumulates chunks until a short packet or ZLP terminates the frame.
const READ_CHUNK_LEN: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// Host -> device half of a transport. Runs behind `&self`: commands are sent
/// from whichever thread holds the client while the reader owns the source.
trait FrameSink: Send + Sync {
    fn send_frame(&self, frame: &[u8], deadline: Instant) -> Result<(), OutOfSync>;
    /// Unblock the paired source, or the reader never returns.
    fn disconnect(&self);
}

/// Device -> host half of a transport, owned by the reader thread.
trait FrameSource: Send {
    /// Next frame body. `None` ends the channel.
    fn next_frame(&mut self) -> Option<Vec<u8>>;
}

/// Debug channel to one Passport, over USB or to a hosted simulator.
///
/// Constructed by [`DebugClient::open`] (USB) or [`DebugClient::connect_sim`].
pub struct DebugClient {
    sink: Box<dyn FrameSink>,
    log_rx: Receiver<Vec<u8>>,
    resp_rx: Receiver<RawResponse>,
    reader_thread: Option<JoinHandle<()>>,
}

impl DebugClient {
    /// Try `PASSPORT_VID:PID`, then fall back to `LEGACY_VID:PID`. If both fail,
    /// returns the error from the primary attempt (legacy is the rarer case).
    pub fn open() -> Result<Self, OpenError> {
        match Self::open_with_vid_pid(PASSPORT_VID, PASSPORT_PID) {
            Ok(client) => Ok(client),
            Err(primary_err) => Self::open_with_vid_pid(LEGACY_VID, LEGACY_PID).map_err(|_| primary_err),
        }
    }

    /// Open a specific VID:PID with no fallback.
    pub fn open_with_vid_pid(vid: u16, pid: u16) -> Result<Self, OpenError> {
        // `mut` is required on some rusb versions (`detach_kernel_driver` takes &mut self)
        // and optional on others; keep it and silence the unused-mut warning.
        #[allow(unused_mut)]
        let mut handle = rusb::open_device_with_vid_pid(vid, pid).ok_or(OpenError::NotFound { vid, pid })?;

        let device = handle.device();
        let config = device.active_config_descriptor().map_err(OpenError::ConfigDescriptor)?;

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

        // The scan only names an interface once both endpoints are in hand, so these cannot be None.
        let (debug_iface, ep_out, ep_in) = match (debug_iface, ep_out, ep_in) {
            (Some(iface), Some(out), Some(in_)) => (iface, out, in_),
            _ => return Err(OpenError::NoDebugInterface),
        };

        // Detach any kernel driver from the debug interface before claiming.
        // Some Linux setups report `kernel_driver_active()` unreliably across
        // the Legacy HID identity transition, so detach on a best-effort basis.
        let _ = handle.detach_kernel_driver(debug_iface);
        handle.claim_interface(debug_iface).map_err(OpenError::ClaimInterface)?;

        let handle = Arc::new(handle);
        let reader_enabled = Arc::new(AtomicBool::new(true));
        let sink = UsbSink { handle: handle.clone(), ep_out, reader_enabled: reader_enabled.clone() };
        let source = UsbSource {
            handle,
            ep_in,
            enabled: reader_enabled,
            buf: vec![0u8; READ_CHUNK_LEN],
            pending: Vec::with_capacity(READ_CHUNK_LEN),
        };
        Ok(Self::from_halves(Box::new(sink), Box::new(source)))
    }

    /// Connect to the debug channel a hosted simulator listens on.
    pub fn connect_sim(addr: &str) -> Result<Self, OpenError> {
        let connect_error = |source| OpenError::SimConnect { addr: addr.to_string(), source };
        let stream = TcpStream::connect(addr).map_err(connect_error)?;
        let reader = stream.try_clone().map_err(connect_error)?;
        Ok(Self::from_halves(Box::new(SimSink(stream)), Box::new(SimSource(reader))))
    }

    fn from_halves(sink: Box<dyn FrameSink>, source: Box<dyn FrameSource>) -> Self {
        let (log_tx, log_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || reader_thread(source, log_tx, resp_tx));
        Self { sink, log_rx, resp_rx, reader_thread: Some(reader_thread) }
    }

    /// Encode `cmd`, send its frame, and wait up to `timeout` for the matching
    /// `[STATUS][PAYLOAD]` response frame. Validates the status byte and
    /// returns just the payload on success.
    ///
    /// # Errors
    ///
    /// `TransportError::OutOfSync` means the exchange itself broke down and the connection has to
    /// be replaced. `Device` and `Locked` mean the device answered and refused, which leaves the
    /// transport usable.
    pub fn send(&self, cmd: Command, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        let resp = self.exchange(cmd, timeout)?;

        match Status::from_byte(resp.status)? {
            Status::Ok => Ok(resp.payload),
            Status::Err => Err(TransportError::Device(resp.status)),
            Status::Locked => Err(TransportError::Locked),
        }
    }

    fn exchange(&self, cmd: Command, timeout: Duration) -> Result<RawResponse, OutOfSync> {
        let mut out_buf = Vec::with_capacity(64);
        cmd.encode_into(&mut out_buf);
        let deadline = Instant::now() + timeout;

        self.sink.send_frame(&out_buf, deadline)?;

        let response_timeout = time_left(deadline, "response")?;
        // The reader drops its sender when the channel ends, which separates a peer that hung up
        // from one that is merely slow to answer.
        self.resp_rx.recv_timeout(response_timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => OutOfSync::NoResponse { cmd: cmd.cmd_byte() },
            RecvTimeoutError::Disconnected => OutOfSync::Closed,
        })
    }

    /// Block up to `timeout` for one log frame. Pass `Duration::ZERO` for a
    /// non-blocking poll. `Disconnected` means the reader thread exited
    /// (transport failure).
    pub fn read_logs(&self, timeout: Duration) -> Result<Vec<u8>, RecvTimeoutError> {
        self.log_rx.recv_timeout(timeout)
    }
}

// For USB, DeviceHandle's Drop calls libusb_close, which releases all claimed
// interfaces. Joining the reader after `disconnect` releases its transport
// half before drop returns, so another process can claim the interface.
impl Drop for DebugClient {
    fn drop(&mut self) {
        self.sink.disconnect();
        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
    }
}

/// Split each device -> host frame to the channel its kind belongs on, until the transport or a
/// receiver closes: either way the client is gone. A frame this build cannot name is dropped
/// rather than resynchronized, since both transports delimit frames outside the payload.
fn reader_thread(mut source: Box<dyn FrameSource>, log_tx: Sender<Vec<u8>>, resp_tx: Sender<RawResponse>) {
    while let Some(frame) = source.next_frame() {
        let Some((&kind, payload)) = frame.split_first() else { continue };
        let delivered = match FrameType::from_byte(kind) {
            Ok(FrameType::Log) => log_tx.send(payload.to_vec()).is_ok(),
            Ok(FrameType::Response) => match payload.split_first() {
                Some((&status, rest)) => resp_tx.send(RawResponse { status, payload: rest.to_vec() }).is_ok(),
                None => true,
            },
            Err(_) => true,
        };
        if !delivered {
            return;
        }
    }
}

// USB transport: frames are delimited by short packets and ZLPs.

struct UsbSink {
    handle: Arc<DeviceHandle<GlobalContext>>,
    ep_out: u8,
    reader_enabled: Arc<AtomicBool>,
}

impl FrameSink for UsbSink {
    fn send_frame(&self, frame: &[u8], deadline: Instant) -> Result<(), OutOfSync> {
        write_bulk_all(frame, deadline, |remaining, write_timeout| {
            self.handle.write_bulk(self.ep_out, remaining, write_timeout).map_err(OutOfSync::Write)
        })?;

        if needs_out_zlp(frame.len()) {
            let write_timeout = time_left(deadline, "bulk OUT ZLP write")?;
            self.handle.write_bulk(self.ep_out, &[], write_timeout).map_err(OutOfSync::Write)?;
        }
        Ok(())
    }

    fn disconnect(&self) { self.reader_enabled.store(false, Ordering::Release); }
}

// The read timeout is what lets the source notice the disabled flag, since a
// bulk read on an idle endpoint would otherwise block indefinitely.
struct UsbSource {
    handle: Arc<DeviceHandle<GlobalContext>>,
    ep_in: u8,
    enabled: Arc<AtomicBool>,
    buf: Vec<u8>,
    pending: Vec<u8>,
}

impl UsbSource {
    fn take_pending(&mut self) -> Option<Vec<u8>> {
        if self.pending.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.pending))
    }
}

impl FrameSource for UsbSource {
    fn next_frame(&mut self) -> Option<Vec<u8>> {
        while self.enabled.load(Ordering::Acquire) {
            match self.handle.read_bulk(self.ep_in, &mut self.buf, READ_TIMEOUT) {
                Ok(0) => {
                    if let Some(frame) = self.take_pending() {
                        return Some(frame);
                    }
                }
                Ok(n) => {
                    self.pending.extend_from_slice(&self.buf[..n]);
                    if self.pending.len() > MAX_FRAME_LEN {
                        self.pending.clear();
                        continue;
                    }
                    if n < READ_CHUNK_LEN {
                        if let Some(frame) = self.take_pending() {
                            return Some(frame);
                        }
                    }
                }
                Err(rusb::Error::Timeout) => continue,
                Err(_) => return None,
            }
        }
        None
    }
}

// Simulator transport: the same frames behind a length prefix (see `stream`).

struct SimSink(TcpStream);

impl FrameSink for SimSink {
    fn send_frame(&self, frame: &[u8], _deadline: Instant) -> Result<(), OutOfSync> {
        // `impl Write for &TcpStream`, so the frame goes out without an exclusive borrow.
        stream::write_frame(&mut (&self.0), frame, &[]).map_err(|e| match e.kind() {
            // A refused or torn-down connection surfaces here when its FIN beats the write.
            std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted => OutOfSync::Closed,
            _ => OutOfSync::StreamWrite(e),
        })
    }

    fn disconnect(&self) { let _ = self.0.shutdown(Shutdown::Both); }
}

struct SimSource(TcpStream);

impl FrameSource for SimSource {
    fn next_frame(&mut self) -> Option<Vec<u8>> { stream::read_frame(&mut self.0).ok().flatten() }
}

fn needs_out_zlp(len: usize) -> bool { len > 0 && len % USB_DEBUG_BULK_MAX_PACKET_LEN == 0 }

fn write_bulk_all(
    bytes: &[u8],
    deadline: Instant,
    mut write: impl FnMut(&[u8], Duration) -> Result<usize, OutOfSync>,
) -> Result<(), OutOfSync> {
    let mut written = 0;
    while written < bytes.len() {
        let write_timeout = time_left(deadline, "bulk OUT write")?;
        let count = write(&bytes[written..], write_timeout)?;
        let remaining = bytes.len() - written;
        if count == 0 {
            return Err(OutOfSync::NoProgress { written, total: bytes.len() });
        }
        if count > remaining {
            return Err(OutOfSync::OverRun { count, remaining });
        }
        if count < remaining && count % USB_DEBUG_BULK_MAX_PACKET_LEN != 0 {
            return Err(OutOfSync::ShortWrite { written: written + count, total: bytes.len() });
        }
        written += count;
    }
    Ok(())
}

fn time_left(deadline: Instant, operation: &'static str) -> Result<Duration, OutOfSync> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(OutOfSync::TimedOut(operation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_write_retries_partial_progress_until_complete() {
        let bytes = vec![0u8; (USB_DEBUG_BULK_MAX_PACKET_LEN * 2) + 76];
        let mut limits = [USB_DEBUG_BULK_MAX_PACKET_LEN, USB_DEBUG_BULK_MAX_PACKET_LEN, 76].into_iter();
        let mut remainders = Vec::new();

        write_bulk_all(&bytes, Instant::now() + Duration::from_secs(1), |remaining, _| {
            remainders.push(remaining.len());
            Ok(limits.next().unwrap().min(remaining.len()))
        })
        .unwrap();

        assert_eq!(
            remainders,
            [(USB_DEBUG_BULK_MAX_PACKET_LEN * 2) + 76, USB_DEBUG_BULK_MAX_PACKET_LEN + 76, 76,]
        );
    }

    #[test]
    fn bulk_write_rejects_a_partial_short_packet() {
        let bytes = vec![0u8; USB_DEBUG_BULK_MAX_PACKET_LEN + 1];
        let error =
            write_bulk_all(&bytes, Instant::now() + Duration::from_secs(1), |_, _| Ok(7)).unwrap_err();

        assert!(matches!(error, OutOfSync::ShortWrite { written: 7, total } if total == bytes.len()));
    }

    #[test]
    fn bulk_write_rejects_zero_progress() {
        let error = write_bulk_all(b"certificate", Instant::now() + Duration::from_secs(1), |_, _| Ok(0))
            .unwrap_err();

        assert!(matches!(error, OutOfSync::NoProgress { written: 0, total: 11 }));
    }

    struct Frames(std::vec::IntoIter<Vec<u8>>);

    impl FrameSource for Frames {
        fn next_frame(&mut self) -> Option<Vec<u8>> { self.0.next() }
    }

    #[test]
    fn reader_routes_frames_to_their_channels() {
        let (log_tx, log_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();

        let mut response = vec![FrameType::Response as u8, Status::Ok as u8];
        response.extend_from_slice(&[0xaa; 4]);
        let mut log = vec![FrameType::Log as u8];
        log.extend_from_slice(b"record");
        // The empty frame and the unknown type are dropped, not fatal: the log frame after them
        // proves the reader kept going.
        let frames = vec![response, vec![], vec![0x7f, 1, 2], log];
        reader_thread(Box::new(Frames(frames.into_iter())), log_tx, resp_tx);

        let resp = resp_rx.try_recv().expect("response");
        assert_eq!(resp.status, Status::Ok as u8);
        assert_eq!(resp.payload, vec![0xaa; 4]);
        assert_eq!(log_rx.try_recv().expect("log"), b"record");
        assert!(log_rx.try_recv().is_err());
        assert!(resp_rx.try_recv().is_err());
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
