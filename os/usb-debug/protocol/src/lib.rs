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

/// Result status for `Command::LaunchApp`.
///
/// The launch response payload starts with the PID as little-endian `u16`.
/// Newer devices append this status byte so host tools can tell whether the
/// app was freshly launched or an existing process was only foregrounded.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive)]
pub enum LaunchAppStatus {
    Launched = 0,
    AlreadyRunning = 1,
}

impl LaunchAppStatus {
    pub fn from_byte(b: u8) -> Result<Self, ProtocolError> {
        Self::from_u8(b).ok_or(ProtocolError::InvalidLaunchAppStatus(b))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchAppResult {
    pub pid: u16,
    pub status: LaunchAppStatus,
}

impl LaunchAppResult {
    pub fn new(pid: u16, status: LaunchAppStatus) -> Self { Self { pid, status } }

    pub fn encode(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(3);
        out.extend_from_slice(&self.pid.to_le_bytes());
        out.push(self.status as u8);
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProtocolError> {
        let bytes: &[u8; 2] = payload.first_chunk().ok_or(ProtocolError::TruncatedPayload {
            cmd: CMD_LAUNCH_APP,
            need: 2,
            got: payload.len(),
        })?;
        let pid = u16::from_le_bytes(*bytes);
        let status = match payload.get(2) {
            Some(status) => LaunchAppStatus::from_byte(*status)?,
            None => LaunchAppStatus::Launched,
        };
        Ok(Self { pid, status })
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
const CMD_LAUNCH_APP: u8 = 0x09;
const CMD_GET_DEVELOPER_MODE: u8 = 0x0a;

/// Host -> device command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Screenshot,
    Tap {
        x: u16,
        y: u16,
        kind: TouchKind,
    },
    PowerButton {
        pressed: bool,
    },
    RebootSamba,
    CloseApp {
        pid: u16,
    },
    KernelCmd {
        cmd_byte: u8,
    },
    InputText(String),
    GetVersion,
    LaunchApp {
        app_id: [u8; 16],
    },
    /// Read the current value of the global `DeveloperMode` setting. Used by
    /// host tools (e.g. `foundation sideload`) to fail early when the user
    /// hasn't enabled Developer Mode on the device.
    GetDeveloperMode,
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
            Command::LaunchApp { .. } => CMD_LAUNCH_APP,
            Command::GetDeveloperMode => CMD_GET_DEVELOPER_MODE,
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
            Command::Screenshot | Command::RebootSamba | Command::GetVersion | Command::GetDeveloperMode => {}
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
            Command::LaunchApp { app_id } => {
                out.extend_from_slice(app_id);
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
            CMD_GET_DEVELOPER_MODE => Ok(Command::GetDeveloperMode),
            CMD_LAUNCH_APP => {
                let bytes: &[u8; 16] = payload.first_chunk().ok_or(ProtocolError::TruncatedPayload {
                    cmd,
                    need: 16,
                    got: payload.len(),
                })?;
                Ok(Command::LaunchApp { app_id: *bytes })
            }
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
    /// Ack carrying a small fixed-size payload (e.g. LaunchApp returning the PID).
    LaunchAck(Vec<u8>),
    /// Reply to `Command::GetDeveloperMode`. Single-byte payload: 0x00 = off, 0x01 = on.
    DeveloperMode(bool),
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
            Response::LaunchAck(d) => (HDR_RESP_OK, d.as_slice()),
            Response::DeveloperMode(enabled) => (HDR_RESP_OK, dev_mode_byte(*enabled)),
            Response::Log(d) => (HDR_LOG, d.as_slice()),
        }
    }
}

// Static one-byte payloads used by `Response::DeveloperMode` so `parts()` can
// return a `&[u8]` slice without allocating or relying on a stack temporary.
const DEV_MODE_ON: &[u8] = &[1];
const DEV_MODE_OFF: &[u8] = &[0];

fn dev_mode_byte(enabled: bool) -> &'static [u8] {
    if enabled {
        DEV_MODE_ON
    } else {
        DEV_MODE_OFF
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
    InvalidLaunchAppStatus(u8),
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
            ProtocolError::InvalidLaunchAppStatus(b) => {
                write!(f, "invalid launch app status 0x{b:02x}")
            }
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
        roundtrip(Command::GetDeveloperMode);
        roundtrip(Command::LaunchApp { app_id: [0xab; 16] });
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

    #[test]
    fn launch_app_result_encodes_status_and_accepts_legacy_payload() {
        let result = LaunchAppResult::new(0x1234, LaunchAppStatus::AlreadyRunning);
        assert_eq!(result.encode(), vec![0x34, 0x12, 1]);
        assert_eq!(LaunchAppResult::decode(&result.encode()).unwrap(), result);
        assert_eq!(
            LaunchAppResult::decode(&[0x34, 0x12]).unwrap(),
            LaunchAppResult::new(0x1234, LaunchAppStatus::Launched)
        );
    }
}
