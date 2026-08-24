// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! USB debug protocol shared between the device-side `usb-debug` service
//! and host-side tooling (`passport-drive`, `keyos-log-viewer`, xtask).
//!
//! Wire format (vendor-specific bulk interface):
//!   OUT (host -> device): `[CMD:1][PAYLOAD:0..N]`
//!   IN  (device -> host): `[FRAME_TYPE:1][PAYLOAD...]`
//!     FRAME_TYPE Log      = 0x01 -- raw 0x1E-terminated log records
//!     FRAME_TYPE Response = 0x02 -- `[STATUS:1][PAYLOAD...]`
//!
//! OUT commands and IN frames are terminated by USB short packets, ZLPs, or the
//! maximum requested transfer length. Keep that framing: it avoids a protocol
//! length prefix and lets the USB transfer boundary carry the packet end marker.
//!
//! The simulator has no USB, so it carries the same frames over a loopback socket, delimited by a
//! length prefix instead (see [`stream`]).
//!
//! Source of truth for command bytes, status bytes, and payload encoding.
//! The `client` feature additionally exposes `DebugClient`, the host-side
//! client over either transport.

use num_derive::FromPrimitive;
use num_traits::FromPrimitive as _;

#[cfg(feature = "client")]
pub mod client;
pub mod stream;

#[cfg(feature = "client")]
pub use client::{
    DebugClient, OpenError, OutOfSync, TransportError, LEGACY_PID, LEGACY_VID, PASSPORT_PID, PASSPORT_VID,
};

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
    Locked = 0x02,
}

impl Status {
    pub fn from_byte(b: u8) -> Result<Self, ProtocolError> {
        Self::from_u8(b).ok_or(ProtocolError::UnknownStatus(b))
    }
}

/// Result status for `Command::LaunchApp`.
///
/// The launch response payload starts with the PID as little-endian `u16`.
/// Newer devices append this status byte so host tools can tell whether the
/// app was freshly launched, an existing process was only foregrounded, or the
/// launch request was rejected with a known reason.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive)]
pub enum LaunchAppStatus {
    Launched = 0,
    AlreadyRunning = 1,
    AppIdNotFound = 2,
    SignatureRejected = 3,
    NoCertificate = 4,
    NotReady = 6,
    InternalError = 7,
    PublisherCertificateExpired = 8,
    PublisherCertificateNotYetActive = 9,
    KeyOsVersionTooOld = 10,
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
const HDR_RESP_LOCKED: &[u8] = &[FrameType::Response as u8, Status::Locked as u8];

// Command byte assignments. Kept private; the `Command` enum is the public API.
const CMD_SCREENSHOT: u8 = 0x01;
const CMD_SWIPE: u8 = 0x02;
const CMD_POWER_BTN: u8 = 0x03;
const CMD_REBOOT_SAMBA: u8 = 0x04;
const CMD_CLOSE_APP: u8 = 0x05;
const CMD_KERNEL_CMD: u8 = 0x06;
const CMD_INPUT_TEXT: u8 = 0x07;
const CMD_GET_VERSION: u8 = 0x08;
const CMD_LAUNCH_APP: u8 = 0x09;
const CMD_GET_DEVELOPER_MODE: u8 = 0x0a;
const CMD_LOAD_APP_BEGIN: u8 = 0x0b;
const CMD_LOAD_APP_FILE_BEGIN: u8 = 0x0c;
const CMD_LOAD_APP_CHUNK: u8 = 0x0d;
const CMD_LOAD_APP_END: u8 = 0x0e;
const CMD_GET_PROCESS_LIST: u8 = 0x0f;
// 0x10 is a retired certificate-import command.
const CMD_GET_ALLOWED_PUBLISHER_COUNT: u8 = 0x11;
// 0x12 is a retired flux-sideload command.
const CMD_INSTALL_CERTIFICATE: u8 = 0x13;
const CMD_GET_SYSTEM_TIME: u8 = 0x14;
const CMD_SET_SYSTEM_TIME: u8 = 0x15;

pub const USB_DEBUG_BULK_MAX_PACKET_LEN: usize = 512;
/// Largest device -> host frame either transport accepts. A screenshot is the big one at
/// 480 x 800 x 4 bytes; the rest of the headroom bounds what a corrupt length prefix can allocate.
pub const MAX_FRAME_LEN: usize = 2 * 1024 * 1024;
/// Maximum host -> device usb-debug transfer size. This is capped by the
/// 16-bit length field in the device USB endpoint API.
pub const USB_DEBUG_OUT_TRANSFER_MAX: usize = u16::MAX as usize;
pub const LOAD_APP_CHUNK_MAX: usize = USB_DEBUG_OUT_TRANSFER_MAX - 1;
/// Maximum UTF-8 bytes for a load-app relative filename. The device reads one
/// bulk max packet for path setup commands: 1 command byte + 8 size bytes
/// + filename. File bytes use the larger `LOAD_APP_CHUNK_MAX` data path.
pub const LOAD_APP_FILE_PATH_MAX: usize = 512 - 1 - 8;
/// Maximum PEM certificate payload accepted for allowed-publisher import.
pub const INSTALL_CERTIFICATE_BYTES_MAX: usize = 16 * 1024;
/// Lowercase hexadecimal bytes in a canonical publisher fingerprint.
pub const PUBLISHER_FINGERPRINT_HEX_LEN: usize = 64;

/// Host -> device command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Screenshot,
    Swipe {
        start_x: u16,
        start_y: u16,
        end_x: u16,
        end_y: u16,
        duration_ms: u16,
        steps: u8,
    },
    PowerButton {
        long: bool,
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
    /// Probe the Developer Mode gated usb-debug interface. If this command is
    /// reachable, Developer Mode is enabled.
    GetDeveloperMode,
    LoadAppBegin {
        app_id: [u8; 16],
    },
    LoadAppFileBegin {
        filename: String,
        size: u64,
    },
    LoadAppChunk(Vec<u8>),
    LoadAppEnd,
    GetProcessList,
    InstallCertificate {
        /// Exact full fingerprint already shown to and accepted by the user.
        expected_fingerprint: [u8; PUBLISHER_FINGERPRINT_HEX_LEN],
        certificate_pem: Vec<u8>,
    },
    GetAllowedPublisherCount,
    /// Read the device wall clock. Certificate validity and anything else time-dependent is judged
    /// against it, so it is the first thing to check when a certificate is rejected.
    GetSystemTime,
    SetSystemTime {
        /// Seconds since the Unix epoch, UTC. Settings renders the local time zone on top of this.
        unix_seconds: u64,
    },
}

impl Command {
    /// Wire CMD byte.
    pub fn cmd_byte(&self) -> u8 {
        match self {
            Command::Screenshot => CMD_SCREENSHOT,
            Command::Swipe { .. } => CMD_SWIPE,
            Command::PowerButton { .. } => CMD_POWER_BTN,
            Command::RebootSamba => CMD_REBOOT_SAMBA,
            Command::CloseApp { .. } => CMD_CLOSE_APP,
            Command::KernelCmd { .. } => CMD_KERNEL_CMD,
            Command::InputText(_) => CMD_INPUT_TEXT,
            Command::GetVersion => CMD_GET_VERSION,
            Command::LaunchApp { .. } => CMD_LAUNCH_APP,
            Command::GetDeveloperMode => CMD_GET_DEVELOPER_MODE,
            Command::LoadAppBegin { .. } => CMD_LOAD_APP_BEGIN,
            Command::LoadAppFileBegin { .. } => CMD_LOAD_APP_FILE_BEGIN,
            Command::LoadAppChunk(_) => CMD_LOAD_APP_CHUNK,
            Command::LoadAppEnd => CMD_LOAD_APP_END,
            Command::GetProcessList => CMD_GET_PROCESS_LIST,
            Command::InstallCertificate { .. } => CMD_INSTALL_CERTIFICATE,
            Command::GetAllowedPublisherCount => CMD_GET_ALLOWED_PUBLISHER_COUNT,
            Command::GetSystemTime => CMD_GET_SYSTEM_TIME,
            Command::SetSystemTime { .. } => CMD_SET_SYSTEM_TIME,
        }
    }

    /// Append `[CMD][PAYLOAD...]` to `out`. Allocates only via `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.cmd_byte());
        match self {
            Command::Screenshot
            | Command::RebootSamba
            | Command::GetVersion
            | Command::GetDeveloperMode
            | Command::LoadAppEnd
            | Command::GetProcessList
            | Command::GetAllowedPublisherCount
            | Command::GetSystemTime => {}
            Command::SetSystemTime { unix_seconds } => {
                out.extend_from_slice(&unix_seconds.to_le_bytes());
            }
            Command::Swipe { start_x, start_y, end_x, end_y, duration_ms, steps } => {
                out.extend_from_slice(&start_x.to_le_bytes());
                out.extend_from_slice(&start_y.to_le_bytes());
                out.extend_from_slice(&end_x.to_le_bytes());
                out.extend_from_slice(&end_y.to_le_bytes());
                out.extend_from_slice(&duration_ms.to_le_bytes());
                out.push(*steps);
            }
            Command::PowerButton { long } => {
                out.push(u8::from(*long));
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
            Command::LoadAppBegin { app_id } => {
                out.extend_from_slice(app_id);
            }
            Command::LoadAppFileBegin { filename, size } => {
                out.extend_from_slice(&size.to_le_bytes());
                out.extend_from_slice(filename.as_bytes());
            }
            Command::LoadAppChunk(data) => {
                out.extend_from_slice(data);
            }
            Command::InstallCertificate { expected_fingerprint, certificate_pem } => {
                out.extend_from_slice(expected_fingerprint);
                out.extend_from_slice(certificate_pem);
            }
        }
    }

    /// Decode `[CMD][PAYLOAD...]`.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let (&cmd, payload) = bytes.split_first().ok_or(ProtocolError::Empty)?;
        match cmd {
            CMD_SCREENSHOT => Ok(Command::Screenshot),
            CMD_SWIPE => {
                let bytes: &[u8; 11] = exact_payload(cmd, payload)?;
                Ok(Command::Swipe {
                    start_x: u16::from_le_bytes([bytes[0], bytes[1]]),
                    start_y: u16::from_le_bytes([bytes[2], bytes[3]]),
                    end_x: u16::from_le_bytes([bytes[4], bytes[5]]),
                    end_y: u16::from_le_bytes([bytes[6], bytes[7]]),
                    duration_ms: u16::from_le_bytes([bytes[8], bytes[9]]),
                    steps: bytes[10],
                })
            }
            CMD_POWER_BTN => {
                let b = *payload.first().ok_or(ProtocolError::TruncatedPayload { cmd, need: 1, got: 0 })?;
                Ok(Command::PowerButton { long: b != 0 })
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
            CMD_LOAD_APP_BEGIN => {
                let bytes: &[u8; 16] = exact_payload(cmd, payload)?;
                Ok(Command::LoadAppBegin { app_id: *bytes })
            }
            CMD_LOAD_APP_FILE_BEGIN => {
                let size_bytes: &[u8; 8] = payload.first_chunk().ok_or(ProtocolError::TruncatedPayload {
                    cmd,
                    need: 8,
                    got: payload.len(),
                })?;
                let size = u64::from_le_bytes(*size_bytes);
                let filename = core::str::from_utf8(&payload[8..]).map_err(|_| ProtocolError::InvalidUtf8)?;
                Ok(Command::LoadAppFileBegin { filename: filename.to_string(), size })
            }
            CMD_LOAD_APP_CHUNK => Ok(Command::LoadAppChunk(payload.to_vec())),
            CMD_LOAD_APP_END => Ok(Command::LoadAppEnd),
            CMD_GET_PROCESS_LIST => Ok(Command::GetProcessList),
            CMD_INSTALL_CERTIFICATE => {
                if payload.len() < PUBLISHER_FINGERPRINT_HEX_LEN {
                    return Err(ProtocolError::TruncatedPayload {
                        cmd,
                        need: PUBLISHER_FINGERPRINT_HEX_LEN + 1,
                        got: payload.len(),
                    });
                }
                let (expected_fingerprint, certificate_pem) = payload.split_at(PUBLISHER_FINGERPRINT_HEX_LEN);
                if certificate_pem.is_empty() {
                    return Err(ProtocolError::InvalidPayloadLength {
                        cmd,
                        need: PUBLISHER_FINGERPRINT_HEX_LEN + 1,
                        got: payload.len(),
                    });
                }
                if !expected_fingerprint.iter().all(u8::is_ascii_hexdigit)
                    || expected_fingerprint.iter().any(u8::is_ascii_uppercase)
                {
                    return Err(ProtocolError::InvalidPublisherFingerprint);
                }
                if certificate_pem.len() > INSTALL_CERTIFICATE_BYTES_MAX {
                    return Err(ProtocolError::InvalidPayloadLength {
                        cmd,
                        need: PUBLISHER_FINGERPRINT_HEX_LEN + INSTALL_CERTIFICATE_BYTES_MAX,
                        got: payload.len(),
                    });
                }
                Ok(Command::InstallCertificate {
                    expected_fingerprint: expected_fingerprint
                        .try_into()
                        .expect("fingerprint slice length checked"),
                    certificate_pem: certificate_pem.to_vec(),
                })
            }
            CMD_GET_ALLOWED_PUBLISHER_COUNT => Ok(Command::GetAllowedPublisherCount),
            CMD_GET_SYSTEM_TIME => Ok(Command::GetSystemTime),
            CMD_SET_SYSTEM_TIME => {
                let bytes: &[u8; 8] = exact_payload(cmd, payload)?;
                Ok(Command::SetSystemTime { unix_seconds: u64::from_le_bytes(*bytes) })
            }
            _ => Err(ProtocolError::UnknownCommand(cmd)),
        }
    }
}

fn exact_payload<const N: usize>(cmd: u8, payload: &[u8]) -> Result<&[u8; N], ProtocolError> {
    if payload.len() < N {
        return Err(ProtocolError::TruncatedPayload { cmd, need: N, got: payload.len() });
    }
    if payload.len() > N {
        return Err(ProtocolError::InvalidPayloadLength { cmd, need: N, got: payload.len() });
    }
    Ok(payload.first_chunk().expect("payload length already checked"))
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
/// intermediate concatenation. `header[0]` is the `FrameType` byte. Remaining
/// header bytes are the first bytes of the frame payload.
#[derive(Debug)]
pub enum Response {
    Ack,
    Err,
    Locked,
    Screenshot(Payload),
    KernelOutput(Payload),
    Version(Vec<u8>),
    /// Ack carrying a small fixed-size payload (e.g. LaunchApp returning the PID).
    LaunchAck(Vec<u8>),
    /// Reply to `Command::GetDeveloperMode`. Single-byte payload: 0x00 = off, 0x01 = on.
    /// Device-side usb-debug returns `true` because the interface itself is Developer Mode gated.
    DeveloperMode(bool),
    /// Reply to `Command::GetAllowedPublisherCount`. Payload is two little-endian `u16`s: how many
    /// stored certificates are usable right now, then how many are stored at all. A device that
    /// holds certificates none of which are usable reports `0, n`, which separates "no publisher
    /// was ever added" from "the ones added cannot be used".
    AllowedPublisherCount(Vec<u8>),
    /// Reply to `Command::GetSystemTime`. Payload is little-endian `u64`: seconds since the Unix
    /// epoch, UTC.
    SystemTime(Vec<u8>),
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
            Response::Locked => (HDR_RESP_LOCKED, &[]),
            Response::Screenshot(p) => (HDR_RESP_OK, p),
            Response::KernelOutput(p) => (HDR_RESP_OK, p),
            Response::Version(d) => (HDR_RESP_OK, d.as_slice()),
            Response::LaunchAck(d) => (HDR_RESP_OK, d.as_slice()),
            Response::DeveloperMode(enabled) => (HDR_RESP_OK, dev_mode_byte(*enabled)),
            Response::AllowedPublisherCount(d) => (HDR_RESP_OK, d.as_slice()),
            Response::SystemTime(d) => (HDR_RESP_OK, d.as_slice()),
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

/// A byte sequence that is not a valid frame. Shared by both ends, since both encode and decode
/// the same wire format; how a command *fared* is the host transport's business, not this.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("empty frame")]
    Empty,
    #[error("unknown command byte 0x{0:02x}")]
    UnknownCommand(u8),
    #[error("unknown frame type 0x{0:02x}")]
    UnknownFrameType(u8),
    #[error("unknown status byte 0x{0:02x}")]
    UnknownStatus(u8),
    #[error("invalid launch app status 0x{0:02x}")]
    InvalidLaunchAppStatus(u8),
    #[error("command 0x{cmd:02x} payload truncated: need {need}, got {got}")]
    TruncatedPayload { cmd: u8, need: usize, got: usize },
    #[error("command 0x{cmd:02x} payload length invalid: need {need}, got {got}")]
    InvalidPayloadLength { cmd: u8, need: usize, got: usize },
    #[error("payload is not valid UTF-8")]
    InvalidUtf8,
    #[error("publisher fingerprint is not 64 lowercase hexadecimal characters")]
    InvalidPublisherFingerprint,
}

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
        roundtrip(Command::Swipe {
            start_x: 100,
            start_y: 930,
            end_x: 100,
            end_y: 20,
            duration_ms: 300,
            steps: 15,
        });
        roundtrip(Command::Swipe {
            start_x: 240,
            start_y: 400,
            end_x: 240,
            end_y: 400,
            duration_ms: 700,
            steps: 0,
        });
        roundtrip(Command::PowerButton { long: false });
        roundtrip(Command::PowerButton { long: true });
        roundtrip(Command::RebootSamba);
        roundtrip(Command::CloseApp { pid: 0x1234 });
        roundtrip(Command::KernelCmd { cmd_byte: b'p' });
        roundtrip(Command::InputText("hello".to_string()));
        roundtrip(Command::InputText(String::new()));
        roundtrip(Command::GetVersion);
        roundtrip(Command::GetDeveloperMode);
        roundtrip(Command::LaunchApp { app_id: [0xab; 16] });
        roundtrip(Command::LoadAppBegin {
            app_id: [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
                0xff,
            ],
        });
        roundtrip(Command::LoadAppFileBegin { filename: "app.elf".to_string(), size: 123 });
        roundtrip(Command::LoadAppFileBegin { filename: "manifest.json".to_string(), size: 456 });
        roundtrip(Command::LoadAppFileBegin { filename: "icon.bin".to_string(), size: 789 });
        roundtrip(Command::LoadAppFileBegin {
            filename: "resources/.foundation/icon.raw".to_string(),
            size: 321,
        });
        roundtrip(Command::LoadAppChunk(vec![1, 2, 3, 4]));
        roundtrip(Command::LoadAppEnd);
        roundtrip(Command::GetProcessList);
        roundtrip(Command::InstallCertificate {
            expected_fingerprint: *b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            certificate_pem: b"-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----\n".to_vec(),
        });
        roundtrip(Command::GetAllowedPublisherCount);
        roundtrip(Command::GetSystemTime);
        roundtrip(Command::SetSystemTime { unix_seconds: 1_754_400_600 });
    }

    #[test]
    fn set_system_time_rejects_a_payload_that_is_not_eight_bytes() {
        let mut short = vec![CMD_SET_SYSTEM_TIME];
        short.extend_from_slice(&[0u8; 7]);
        assert!(Command::decode(&short).is_err());

        let mut long = vec![CMD_SET_SYSTEM_TIME];
        long.extend_from_slice(&[0u8; 9]);
        assert!(Command::decode(&long).is_err());
    }

    #[test]
    fn load_app_chunk_fills_max_out_transfer() {
        assert_eq!(LOAD_APP_CHUNK_MAX + 1, USB_DEBUG_OUT_TRANSFER_MAX);
        assert!(USB_DEBUG_OUT_TRANSFER_MAX <= u16::MAX as usize);
    }

    #[test]
    fn install_certificate_payload_is_capped() {
        let mut bytes = Vec::from([CMD_INSTALL_CERTIFICATE]);
        bytes.extend_from_slice(b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        bytes.extend(vec![b'a'; INSTALL_CERTIFICATE_BYTES_MAX]);
        assert!(matches!(Command::decode(&bytes), Ok(Command::InstallCertificate { .. })));

        bytes.push(b'a');
        assert!(matches!(
            Command::decode(&bytes),
            Err(ProtocolError::InvalidPayloadLength {
                cmd: CMD_INSTALL_CERTIFICATE,
                need,
                got
            }) if need == PUBLISHER_FINGERPRINT_HEX_LEN + INSTALL_CERTIFICATE_BYTES_MAX
                && got == PUBLISHER_FINGERPRINT_HEX_LEN + INSTALL_CERTIFICATE_BYTES_MAX + 1
        ));

        assert!(matches!(
            Command::decode(&[CMD_INSTALL_CERTIFICATE]),
            Err(ProtocolError::TruncatedPayload {
                cmd: CMD_INSTALL_CERTIFICATE,
                need,
                got: 0
            }) if need == PUBLISHER_FINGERPRINT_HEX_LEN + 1
        ));
    }

    #[test]
    fn install_certificate_requires_a_lowercase_hex_fingerprint() {
        let mut bytes = Vec::from([CMD_INSTALL_CERTIFICATE]);
        bytes.extend_from_slice(b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeF");
        bytes.extend_from_slice(b"certificate");

        assert!(matches!(Command::decode(&bytes), Err(ProtocolError::InvalidPublisherFingerprint)));
    }

    #[test]
    fn decode_rejects_truncated() {
        assert!(matches!(Command::decode(&[]), Err(ProtocolError::Empty)));
        assert!(matches!(
            Command::decode(&[CMD_SWIPE, 0, 0]),
            Err(ProtocolError::TruncatedPayload { cmd: CMD_SWIPE, need: 11, got: 2 })
        ));
        assert!(matches!(
            Command::decode(&[CMD_CLOSE_APP, 0]),
            Err(ProtocolError::TruncatedPayload { cmd: CMD_CLOSE_APP, need: 2, got: 1 })
        ));
        assert!(matches!(
            Command::decode(&[CMD_POWER_BTN]),
            Err(ProtocolError::TruncatedPayload { cmd: CMD_POWER_BTN, .. })
        ));
        assert!(matches!(
            Command::decode(&[CMD_LOAD_APP_FILE_BEGIN, 0]),
            Err(ProtocolError::TruncatedPayload { cmd: CMD_LOAD_APP_FILE_BEGIN, need: 8, got: 1 })
        ));
        assert!(matches!(
            Command::decode(&[CMD_LAUNCH_APP, 0, 1]),
            Err(ProtocolError::TruncatedPayload { cmd: CMD_LAUNCH_APP, need: 16, got: 2 })
        ));
        assert!(matches!(
            Command::decode(&[CMD_LOAD_APP_BEGIN, 0, 1]),
            Err(ProtocolError::TruncatedPayload { cmd: CMD_LOAD_APP_BEGIN, need: 16, got: 2 })
        ));
    }

    #[test]
    fn load_app_begin_rejects_legacy_hex_string_payload() {
        let mut bytes = Vec::from([CMD_LOAD_APP_BEGIN]);
        bytes.extend_from_slice(b"00112233445566778899aabbccddeeff");
        assert!(matches!(
            Command::decode(&bytes),
            Err(ProtocolError::InvalidPayloadLength { cmd: CMD_LOAD_APP_BEGIN, need: 16, got: 32 })
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
        let failure = LaunchAppResult::new(0, LaunchAppStatus::NoCertificate);
        assert_eq!(LaunchAppResult::decode(&failure.encode()).unwrap(), failure);
        for status in [LaunchAppStatus::KeyOsVersionTooOld] {
            let failure = LaunchAppResult::new(0, status);
            assert_eq!(LaunchAppResult::decode(&failure.encode()).unwrap(), failure);
        }
        assert_eq!(
            LaunchAppResult::decode(&[0x34, 0x12]).unwrap(),
            LaunchAppResult::new(0x1234, LaunchAppStatus::Launched)
        );
    }

    #[test]
    fn locked_response_uses_locked_status() {
        let (header, payload) = Response::Locked.parts();

        assert_eq!(header, &[FrameType::Response as u8, Status::Locked as u8]);
        assert!(payload.is_empty());
    }
}
