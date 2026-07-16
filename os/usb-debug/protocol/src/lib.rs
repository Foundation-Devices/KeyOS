// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! USB debug protocol shared between the device-side `usb-debug` service
//! and host-side tooling (`passport-drive`, `keyos-log-viewer`, xtask).
//!
//! Wire format (vendor-specific bulk interface, single transfer per frame):
//!   OUT (host -> device): `[CMD:1][PAYLOAD:0..N]`
//!   IN  (device -> host): `[FRAME_TYPE:1][PAYLOAD...]`
//!     FRAME_TYPE Log      = 0x01 -- raw 0x1E-terminated log records
//!     FRAME_TYPE Response = 0x02 -- `[STATUS:1][PAYLOAD...]`
//!
//! Source of truth for command bytes, status bytes, and payload encoding.
//! The `client` feature additionally exposes `UsbDebugClient`, a `rusb`-based
//! host transport.

use num_derive::FromPrimitive;
use num_traits::FromPrimitive as _;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "client")]
pub use client::{UsbDebugClient, LEGACY_PID, LEGACY_VID, PASSPORT_PID, PASSPORT_VID};

/// First byte of every device -> host transfer.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive)]
pub enum FrameType {
    Log = 0x01,
    Response = 0x02,
}

impl FrameType {
    pub fn from_byte(b: u8) -> Result<Self, ProtocolError> {
        Self::from_u8(b).ok_or(ProtocolError::UnknownFrameType(b))
    }
}

/// Second byte of every Response frame.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive)]
pub enum Status {
    Ok = 0x00,
    Err = 0x01,
}

impl Status {
    pub fn from_byte(b: u8) -> Result<Self, ProtocolError> {
        Self::from_u8(b).ok_or(ProtocolError::UnknownStatus(b))
    }
}

/// Touch event kind for `Command::Tap`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive)]
pub enum TouchKind {
    Press = 0,
    Release = 1,
    Drag = 2,
}

impl TouchKind {
    pub fn from_byte(b: u8) -> Result<Self, ProtocolError> {
        Self::from_u8(b).ok_or(ProtocolError::InvalidTouchKind(b))
    }
}

// Static frame headers so `Response::parts` can return `&[u8]` without
// allocating or relying on temporary stack arrays.
const HDR_LOG: &[u8] = &[FrameType::Log as u8];
const HDR_RESP_OK: &[u8] = &[FrameType::Response as u8, Status::Ok as u8];
const HDR_RESP_ERR: &[u8] = &[FrameType::Response as u8, Status::Err as u8];

// Command byte assignments. Kept private; the `Command` enum is the public API.
const CMD_SCREENSHOT: u8 = 0x01;
const CMD_TAP: u8 = 0x02;
const CMD_POWER_BTN: u8 = 0x03;
const CMD_REBOOT_SAMBA: u8 = 0x04;
const CMD_CLOSE_APP: u8 = 0x05;
const CMD_KERNEL_CMD: u8 = 0x06;
const CMD_INPUT_TEXT: u8 = 0x07;
const CMD_GET_VERSION: u8 = 0x08;

/// Host -> device command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Screenshot,
    Tap { x: u16, y: u16, kind: TouchKind },
    PowerButton { pressed: bool },
    RebootSamba,
    CloseApp { pid: u16 },
    KernelCmd { cmd_byte: u8 },
    InputText(String),
    GetVersion,
}

impl Command {
    /// Wire CMD byte.
    pub fn cmd_byte(&self) -> u8 {
        match self {
            Command::Screenshot => CMD_SCREENSHOT,
            Command::Tap { .. } => CMD_TAP,
            Command::PowerButton { .. } => CMD_POWER_BTN,
            Command::RebootSamba => CMD_REBOOT_SAMBA,
            Command::CloseApp { .. } => CMD_CLOSE_APP,
            Command::KernelCmd { .. } => CMD_KERNEL_CMD,
            Command::InputText(_) => CMD_INPUT_TEXT,
            Command::GetVersion => CMD_GET_VERSION,
        }
    }

    /// Upper bound on the response payload size (excluding the 2-byte response
    /// header). Used by the client to size its read buffer.
    pub fn max_response_size(&self) -> usize {
        match self {
            // 480 * 800 * 4 = 1,536,000 plus header slack.
            Command::Screenshot => 2 * 1024 * 1024,
            // Kernel debug output is bounded by the kernel's debug buffer.
            Command::KernelCmd { .. } => 256 * 1024,
            // Everything else: ack or a short string.
            _ => 4 * 1024,
        }
    }

    /// Append `[CMD][PAYLOAD...]` to `out`. Allocates only via `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.cmd_byte());
        match self {
            Command::Screenshot | Command::RebootSamba | Command::GetVersion => {}
            Command::Tap { x, y, kind } => {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                out.push(*kind as u8);
            }
            Command::PowerButton { pressed } => {
                out.push(u8::from(*pressed));
            }
            Command::CloseApp { pid } => {
                out.extend_from_slice(&pid.to_le_bytes());
            }
            Command::KernelCmd { cmd_byte } => {
                out.push(*cmd_byte);
            }
            Command::InputText(text) => {
                out.extend_from_slice(text.as_bytes());
            }
        }
    }

    /// Decode `[CMD][PAYLOAD...]`.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let (&cmd, payload) = bytes.split_first().ok_or(ProtocolError::Empty)?;
        match cmd {
            CMD_SCREENSHOT => Ok(Command::Screenshot),
            CMD_TAP => {
                let bytes: &[u8; 5] = payload.first_chunk().ok_or(ProtocolError::TruncatedPayload {
                    cmd,
                    need: 5,
                    got: payload.len(),
                })?;
                let x = u16::from_le_bytes([bytes[0], bytes[1]]);
                let y = u16::from_le_bytes([bytes[2], bytes[3]]);
                let kind = TouchKind::from_byte(bytes[4])?;
                Ok(Command::Tap { x, y, kind })
            }
            CMD_POWER_BTN => {
                let b = *payload.first().ok_or(ProtocolError::TruncatedPayload { cmd, need: 1, got: 0 })?;
                Ok(Command::PowerButton { pressed: b != 0 })
            }
            CMD_REBOOT_SAMBA => Ok(Command::RebootSamba),
            CMD_CLOSE_APP => {
                let bytes: &[u8; 2] = payload.first_chunk().ok_or(ProtocolError::TruncatedPayload {
                    cmd,
                    need: 2,
                    got: payload.len(),
                })?;
                Ok(Command::CloseApp { pid: u16::from_le_bytes(*bytes) })
            }
            CMD_KERNEL_CMD => {
                let cmd_byte =
                    *payload.first().ok_or(ProtocolError::TruncatedPayload { cmd, need: 1, got: 0 })?;
                Ok(Command::KernelCmd { cmd_byte })
            }
            CMD_INPUT_TEXT => {
                let text = core::str::from_utf8(payload).map_err(|_| ProtocolError::InvalidUtf8)?;
                Ok(Command::InputText(text.to_string()))
            }
            CMD_GET_VERSION => Ok(Command::GetVersion),
            _ => Err(ProtocolError::UnknownCommand(cmd)),
        }
    }
}

/// Response payload buffer. On keyos, wraps a `DropDeallocate` (typically the
/// gui-server-lent capture buffer or the kernel debug command buffer) plus a
/// length, so the writer thread can read directly from mapped pages without
/// an intermediate memcpy. Off-target (simulator/tests), holds a `Vec<u8>`.
///
/// Always derefs to the meaningful `&[u8]` slice; callers don't need to know
/// which representation is in use.
pub struct Payload {
    #[cfg(keyos)]
    buf: xous::DropDeallocate,
    #[cfg(keyos)]
    len: usize,
    #[cfg(not(keyos))]
    bytes: Vec<u8>,
}

impl Payload {
    /// Wrap a mapped memory region with a meaningful length. `len` may be less
    /// than the region size (e.g. kernel debug output) -- the trailing bytes
    /// are ignored.
    #[cfg(keyos)]
    pub fn from_mapped(buf: xous::DropDeallocate, len: usize) -> Self { Self { buf, len } }

    /// Wrap an owned byte buffer.
    #[cfg(not(keyos))]
    pub fn from_vec(bytes: Vec<u8>) -> Self { Self { bytes } }
}

impl core::ops::Deref for Payload {
    type Target = [u8];

    #[cfg(keyos)]
    fn deref(&self) -> &[u8] { &self.buf.as_slice::<u8>()[..self.len] }

    #[cfg(not(keyos))]
    fn deref(&self) -> &[u8] { &self.bytes }
}

impl core::fmt::Debug for Payload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Payload").field("len", &self.len()).finish()
    }
}

/// Device -> host. Constructed by the device dispatcher.
///
/// `parts()` returns `(header, payload)` slices for direct USB writes without
/// intermediate concatenation.
#[derive(Debug)]
pub enum Response {
    Ack,
    Err,
    Screenshot(Payload),
    KernelOutput(Payload),
    Version(Vec<u8>),
    /// Asynchronous log frame; not a reply to a `Command`.
    Log(Vec<u8>),
}

impl Response {
    /// Header + payload as separate slices. The header includes the leading
    /// FrameType byte (and Status byte for replies).
    pub fn parts(&self) -> (&[u8], &[u8]) {
        match self {
            Response::Ack => (HDR_RESP_OK, &[]),
            Response::Err => (HDR_RESP_ERR, &[]),
            Response::Screenshot(p) => (HDR_RESP_OK, p),
            Response::KernelOutput(p) => (HDR_RESP_OK, p),
            Response::Version(d) => (HDR_RESP_OK, d.as_slice()),
            Response::Log(d) => (HDR_LOG, d.as_slice()),
        }
    }
}

/// Wire-level response as received: status byte + payload bytes. Host call
/// sites typically check `status` and interpret `payload` according to the
/// command that was sent.
#[derive(Debug)]
pub struct RawResponse {
    pub status: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub enum ProtocolError {
    Empty,
    UnknownCommand(u8),
    UnknownFrameType(u8),
    UnknownStatus(u8),
    InvalidTouchKind(u8),
    TruncatedPayload {
        cmd: u8,
        need: usize,
        got: usize,
    },
    InvalidUtf8,
    /// Returned by `UsbDebugClient::send_checked` when the device replied with
    /// `Status::Err` (or any non-Ok status byte).
    DeviceError(u8),
}

impl core::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProtocolError::Empty => write!(f, "empty frame"),
            ProtocolError::UnknownCommand(b) => write!(f, "unknown command byte 0x{b:02x}"),
            ProtocolError::UnknownFrameType(b) => write!(f, "unknown frame type 0x{b:02x}"),
            ProtocolError::UnknownStatus(b) => write!(f, "unknown status byte 0x{b:02x}"),
            ProtocolError::InvalidTouchKind(b) => write!(f, "invalid touch kind 0x{b:02x}"),
            ProtocolError::TruncatedPayload { cmd, need, got } => {
                write!(f, "command 0x{cmd:02x} payload truncated: need {need}, got {got}")
            }
            ProtocolError::InvalidUtf8 => write!(f, "payload is not valid UTF-8"),
            ProtocolError::DeviceError(b) => write!(f, "device returned status 0x{b:02x}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(cmd: Command) {
        let mut buf = Vec::new();
        cmd.encode_into(&mut buf);
        let decoded = Command::decode(&buf).expect("decode");
        assert_eq!(cmd, decoded);
    }

    #[test]
    fn command_roundtrips() {
        roundtrip(Command::Screenshot);
        roundtrip(Command::Tap { x: 480, y: 800, kind: TouchKind::Press });
        roundtrip(Command::Tap { x: 0, y: 0, kind: TouchKind::Release });
        roundtrip(Command::Tap { x: 12, y: 34, kind: TouchKind::Drag });
        roundtrip(Command::PowerButton { pressed: true });
        roundtrip(Command::PowerButton { pressed: false });
        roundtrip(Command::RebootSamba);
        roundtrip(Command::CloseApp { pid: 0x1234 });
        roundtrip(Command::KernelCmd { cmd_byte: b'p' });
        roundtrip(Command::InputText("hello".to_string()));
        roundtrip(Command::InputText(String::new()));
        roundtrip(Command::GetVersion);
    }

    #[test]
    fn decode_rejects_truncated() {
        assert!(matches!(Command::decode(&[]), Err(ProtocolError::Empty)));
        assert!(matches!(
            Command::decode(&[CMD_TAP, 0, 0]),
            Err(ProtocolError::TruncatedPayload { cmd: CMD_TAP, need: 5, got: 2 })
        ));
        assert!(matches!(
            Command::decode(&[CMD_CLOSE_APP, 0]),
            Err(ProtocolError::TruncatedPayload { cmd: CMD_CLOSE_APP, need: 2, got: 1 })
        ));
        assert!(matches!(
            Command::decode(&[CMD_POWER_BTN]),
            Err(ProtocolError::TruncatedPayload { cmd: CMD_POWER_BTN, .. })
        ));
    }

    #[test]
    fn decode_rejects_unknown_command() {
        assert!(matches!(Command::decode(&[0xFE]), Err(ProtocolError::UnknownCommand(0xFE))));
    }

    #[test]
    fn input_text_roundtrip_utf8() {
        let mut buf = Vec::new();
        Command::InputText("héllo, world".to_string()).encode_into(&mut buf);
        let decoded = Command::decode(&buf).unwrap();
        assert_eq!(decoded, Command::InputText("héllo, world".to_string()));
    }

    #[test]
    fn input_text_rejects_invalid_utf8() {
        let bytes = &[CMD_INPUT_TEXT, 0xff, 0xfe];
        assert!(matches!(Command::decode(bytes), Err(ProtocolError::InvalidUtf8)));
    }
}
