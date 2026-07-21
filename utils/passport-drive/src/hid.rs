// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-side HID transport for sending APDUs/messages to Passport Prime.
//!
//! Supports two modes depending on the device's current VID:
//!
//! - **Legacy mode** (VID=0x2c97): Uses the Legacy HID framing protocol (channel 0x0101, tag 0x05). Active
//!   when a Flux/Legacy app is running.
//!
//! - **CTAP/FIDO mode** (VID=0x1307): Uses the CTAPHID protocol (FIDO HID). U2F messages are wrapped in
//!   CTAPHID_MSG (0x03) frames.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};

/// HID report size in bytes.
const REPORT_SIZE: usize = 64;

// ─── Legacy HID framing ─────────────────────────────────────────────────────

const LEGACY_TAG_APDU: u8 = 0x05;
const LEGACY_INIT_HEADER_LEN: usize = 7; // channel:2 + tag:1 + seq:2 + len:2
const LEGACY_CONT_HEADER_LEN: usize = 5; // channel:2 + tag:1 + seq:2
const LEGACY_INIT_DATA_CAPACITY: usize = REPORT_SIZE - LEGACY_INIT_HEADER_LEN;
const LEGACY_CONT_DATA_CAPACITY: usize = REPORT_SIZE - LEGACY_CONT_HEADER_LEN;
const LEGACY_CHANNEL_ID: u16 = 0x0101;

const LEGACY_VID: u16 = 0x2c97;
const LEGACY_USAGE_PAGE: u16 = 0xFFA0;

// ─── CTAPHID framing ────────────────────────────────────────────────────────

const CTAPHID_BROADCAST_CID: u32 = 0xFFFFFFFF;
const CTAPHID_INIT: u8 = 0x06;
const CTAPHID_MSG: u8 = 0x03;
const CTAPHID_INIT_HEADER_LEN: usize = 7; // cid:4 + cmd:1 + len:2
const CTAPHID_CONT_HEADER_LEN: usize = 5; // cid:4 + seq:1
const CTAPHID_INIT_DATA_CAPACITY: usize = REPORT_SIZE - CTAPHID_INIT_HEADER_LEN;
const CTAPHID_CONT_DATA_CAPACITY: usize = REPORT_SIZE - CTAPHID_CONT_HEADER_LEN;

const PASSPORT_VID: u16 = 0x1307;
const FIDO_USAGE_PAGE: u16 = 0xF1D0;

// ─── Device detection ───────────────────────────────────────────────────────

/// Which HID protocol the device is using.
pub enum HidMode {
    Legacy,
    Fido,
}

/// Open the HID device, auto-detecting the mode.
/// Returns the device handle and the detected mode.
pub fn open_hid() -> Result<(HidDevice, HidMode)> {
    let api = HidApi::new().context("Failed to initialize HID API")?;

    // Try Legacy first (Flux app running)
    for info in api.device_list() {
        if info.vendor_id() == LEGACY_VID && info.usage_page() == LEGACY_USAGE_PAGE {
            let device = info
                .open_device(&api)
                .with_context(|| format!("Failed to open Legacy HID at {}", info.path().to_string_lossy()))?;
            return Ok((device, HidMode::Legacy));
        }
    }

    // Try CTAP/FIDO (normal mode)
    for info in api.device_list() {
        if info.vendor_id() == PASSPORT_VID && info.usage_page() == FIDO_USAGE_PAGE {
            let device = info
                .open_device(&api)
                .with_context(|| format!("Failed to open FIDO HID at {}", info.path().to_string_lossy()))?;
            return Ok((device, HidMode::Fido));
        }
    }

    bail!(
        "No Passport HID device found. Tried Legacy (VID={LEGACY_VID:#06x}, usage={LEGACY_USAGE_PAGE:#06x}) \
         and FIDO (VID={PASSPORT_VID:#06x}, usage={FIDO_USAGE_PAGE:#06x})."
    )
}

// ─── APDU exchange (auto-detects framing) ───────────────────────────────────

/// Exchange an APDU with the device, using the appropriate framing for the mode.
pub fn exchange_apdu(device: &HidDevice, apdu: &[u8], timeout_ms: i32) -> Result<Vec<u8>> {
    // Peek at the device info to determine mode.
    // We check the device's VID via the HidDevice info.
    let info = device.get_device_info().context("Failed to get HID device info")?;
    if info.vendor_id() == LEGACY_VID {
        exchange_legacy_apdu(device, apdu, timeout_ms)
    } else {
        exchange_ctaphid_msg(device, apdu, timeout_ms)
    }
}

// ─── Legacy HID framing ─────────────────────────────────────────────────────

fn legacy_fragment(apdu: &[u8]) -> Vec<[u8; REPORT_SIZE]> {
    let total_len = apdu.len();
    let mut reports = Vec::new();
    let mut offset = 0;
    let mut seq: u16 = 0;

    // Initialization report
    let mut report = [0u8; REPORT_SIZE];
    report[0..2].copy_from_slice(&LEGACY_CHANNEL_ID.to_be_bytes());
    report[2] = LEGACY_TAG_APDU;
    report[3..5].copy_from_slice(&0u16.to_be_bytes());
    report[5..7].copy_from_slice(&(total_len as u16).to_be_bytes());
    let chunk = total_len.min(LEGACY_INIT_DATA_CAPACITY);
    report[LEGACY_INIT_HEADER_LEN..LEGACY_INIT_HEADER_LEN + chunk].copy_from_slice(&apdu[..chunk]);
    offset += chunk;
    reports.push(report);
    seq += 1;

    while offset < total_len {
        let mut report = [0u8; REPORT_SIZE];
        report[0..2].copy_from_slice(&LEGACY_CHANNEL_ID.to_be_bytes());
        report[2] = LEGACY_TAG_APDU;
        report[3..5].copy_from_slice(&seq.to_be_bytes());
        let chunk = (total_len - offset).min(LEGACY_CONT_DATA_CAPACITY);
        report[LEGACY_CONT_HEADER_LEN..LEGACY_CONT_HEADER_LEN + chunk]
            .copy_from_slice(&apdu[offset..offset + chunk]);
        offset += chunk;
        reports.push(report);
        seq += 1;
    }

    reports
}

struct LegacyReassembler {
    buf: Vec<u8>,
    remaining: usize,
    expected_seq: u16,
}

impl LegacyReassembler {
    fn new() -> Self { Self { buf: Vec::new(), remaining: 0, expected_seq: 0 } }

    fn feed(&mut self, report: &[u8]) -> Result<Option<Vec<u8>>> {
        if report.len() < LEGACY_CONT_HEADER_LEN {
            bail!("Report too short: {} bytes", report.len());
        }

        let tag = report[2];
        let seq = u16::from_be_bytes([report[3], report[4]]);

        if tag != LEGACY_TAG_APDU {
            bail!("Invalid tag: expected 0x05, got 0x{tag:02x}");
        }

        if seq == 0 {
            if report.len() < LEGACY_INIT_HEADER_LEN {
                bail!("Init packet too short: {} bytes", report.len());
            }
            let total_len = u16::from_be_bytes([report[5], report[6]]) as usize;
            self.buf.clear();
            self.buf.reserve(total_len);
            let to_copy =
                (report.len() - LEGACY_INIT_HEADER_LEN).min(LEGACY_INIT_DATA_CAPACITY).min(total_len);
            self.buf.extend_from_slice(&report[LEGACY_INIT_HEADER_LEN..LEGACY_INIT_HEADER_LEN + to_copy]);
            self.remaining = total_len.saturating_sub(to_copy);
            self.expected_seq = 1;
        } else {
            if seq != self.expected_seq {
                bail!("Sequence mismatch: expected {}, got {seq}", self.expected_seq);
            }
            let to_copy =
                (report.len() - LEGACY_CONT_HEADER_LEN).min(LEGACY_CONT_DATA_CAPACITY).min(self.remaining);
            self.buf.extend_from_slice(&report[LEGACY_CONT_HEADER_LEN..LEGACY_CONT_HEADER_LEN + to_copy]);
            self.remaining = self.remaining.saturating_sub(to_copy);
            self.expected_seq += 1;
        }

        if self.remaining == 0 && !self.buf.is_empty() {
            let data = core::mem::take(&mut self.buf);
            self.expected_seq = 0;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    }
}

fn exchange_legacy_apdu(device: &HidDevice, apdu: &[u8], timeout_ms: i32) -> Result<Vec<u8>> {
    let reports = legacy_fragment(apdu);
    for report in &reports {
        let mut buf = Vec::with_capacity(1 + REPORT_SIZE);
        buf.push(0x00);
        buf.extend_from_slice(report);
        device.write(&buf).context("Failed to write Legacy HID report")?;
    }

    let mut reassembler = LegacyReassembler::new();
    let mut read_buf = [0u8; REPORT_SIZE];
    loop {
        let n = device.read_timeout(&mut read_buf, timeout_ms).context("Legacy HID read error")?;
        if n == 0 {
            bail!("Legacy HID read timeout ({timeout_ms}ms)");
        }
        if let Some(rapdu) = reassembler.feed(&read_buf[..n])? {
            return Ok(rapdu);
        }
    }
}

// ─── CTAPHID framing ────────────────────────────────────────────────────────

fn ctaphid_fragment(cid: u32, cmd: u8, payload: &[u8]) -> Vec<[u8; REPORT_SIZE]> {
    let total_len = payload.len();
    let mut reports = Vec::new();
    let mut offset = 0;
    let mut seq: u8 = 0;

    // Initialization report: [CID:4][CMD|0x80:1][LEN:2][DATA...]
    let mut report = [0u8; REPORT_SIZE];
    report[0..4].copy_from_slice(&cid.to_be_bytes());
    report[4] = cmd | 0x80; // Bit 7 set = initialization packet
    report[5..7].copy_from_slice(&(total_len as u16).to_be_bytes());
    let chunk = total_len.min(CTAPHID_INIT_DATA_CAPACITY);
    report[CTAPHID_INIT_HEADER_LEN..CTAPHID_INIT_HEADER_LEN + chunk].copy_from_slice(&payload[..chunk]);
    offset += chunk;
    reports.push(report);

    // Continuation reports: [CID:4][SEQ:1][DATA...]
    while offset < total_len {
        let mut report = [0u8; REPORT_SIZE];
        report[0..4].copy_from_slice(&cid.to_be_bytes());
        report[4] = seq; // Bit 7 clear = continuation
        let chunk = (total_len - offset).min(CTAPHID_CONT_DATA_CAPACITY);
        report[CTAPHID_CONT_HEADER_LEN..CTAPHID_CONT_HEADER_LEN + chunk]
            .copy_from_slice(&payload[offset..offset + chunk]);
        offset += chunk;
        reports.push(report);
        seq += 1;
    }

    reports
}

struct CtaphidReassembler {
    buf: Vec<u8>,
    remaining: usize,
    expected_seq: u8,
    cmd: u8,
}

impl CtaphidReassembler {
    fn new() -> Self { Self { buf: Vec::new(), remaining: 0, expected_seq: 0, cmd: 0 } }

    fn feed(&mut self, report: &[u8]) -> Result<Option<(u8, Vec<u8>)>> {
        if report.len() < CTAPHID_CONT_HEADER_LEN {
            bail!("CTAPHID report too short: {} bytes", report.len());
        }

        let cmd_or_seq = report[4];

        if cmd_or_seq & 0x80 != 0 {
            // Initialization packet
            if report.len() < CTAPHID_INIT_HEADER_LEN {
                bail!("CTAPHID init packet too short");
            }
            self.cmd = cmd_or_seq & 0x7F;
            let total_len = u16::from_be_bytes([report[5], report[6]]) as usize;
            self.buf.clear();
            self.buf.reserve(total_len);
            let to_copy =
                (report.len() - CTAPHID_INIT_HEADER_LEN).min(CTAPHID_INIT_DATA_CAPACITY).min(total_len);
            self.buf.extend_from_slice(&report[CTAPHID_INIT_HEADER_LEN..CTAPHID_INIT_HEADER_LEN + to_copy]);
            self.remaining = total_len.saturating_sub(to_copy);
            self.expected_seq = 0;
        } else {
            // Continuation packet
            let seq = cmd_or_seq;
            if seq != self.expected_seq {
                bail!("CTAPHID seq mismatch: expected {}, got {seq}", self.expected_seq);
            }
            let to_copy =
                (report.len() - CTAPHID_CONT_HEADER_LEN).min(CTAPHID_CONT_DATA_CAPACITY).min(self.remaining);
            self.buf.extend_from_slice(&report[CTAPHID_CONT_HEADER_LEN..CTAPHID_CONT_HEADER_LEN + to_copy]);
            self.remaining = self.remaining.saturating_sub(to_copy);
            self.expected_seq += 1;
        }

        if self.remaining == 0 && !self.buf.is_empty() {
            let data = core::mem::take(&mut self.buf);
            self.expected_seq = 0;
            Ok(Some((self.cmd, data)))
        } else {
            Ok(None)
        }
    }
}

/// Allocate a CTAPHID channel via CTAPHID_INIT.
fn ctaphid_init(device: &HidDevice, timeout_ms: i32) -> Result<u32> {
    // Send INIT with 8-byte random nonce on broadcast channel.
    let nonce: [u8; 8] = [0x42; 8]; // fixed nonce is fine for our use
    let reports = ctaphid_fragment(CTAPHID_BROADCAST_CID, CTAPHID_INIT, &nonce);
    for report in &reports {
        let mut buf = Vec::with_capacity(1 + REPORT_SIZE);
        buf.push(0x00);
        buf.extend_from_slice(report);
        device.write(&buf).context("CTAPHID_INIT write")?;
    }

    let mut reassembler = CtaphidReassembler::new();
    let mut read_buf = [0u8; REPORT_SIZE];
    loop {
        let n = device.read_timeout(&mut read_buf, timeout_ms).context("CTAPHID_INIT read")?;
        if n == 0 {
            bail!("CTAPHID_INIT timeout");
        }
        if let Some((cmd, data)) = reassembler.feed(&read_buf[..n])? {
            if cmd != CTAPHID_INIT {
                bail!("Expected CTAPHID_INIT response, got cmd 0x{cmd:02x}");
            }
            if data.len() < 17 {
                bail!("CTAPHID_INIT response too short: {} bytes", data.len());
            }
            // Response: nonce[8] + cid[4] + version + major + minor + build + capabilities
            let cid = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
            return Ok(cid);
        }
    }
}

/// Exchange a U2F/FIDO message via CTAPHID_MSG.
fn exchange_ctaphid_msg(device: &HidDevice, apdu: &[u8], timeout_ms: i32) -> Result<Vec<u8>> {
    // Allocate a channel first.
    let cid = ctaphid_init(device, timeout_ms)?;

    // Send the APDU wrapped in CTAPHID_MSG.
    let reports = ctaphid_fragment(cid, CTAPHID_MSG, apdu);
    for report in &reports {
        let mut buf = Vec::with_capacity(1 + REPORT_SIZE);
        buf.push(0x00);
        buf.extend_from_slice(report);
        device.write(&buf).context("CTAPHID_MSG write")?;
    }

    let mut reassembler = CtaphidReassembler::new();
    let mut read_buf = [0u8; REPORT_SIZE];
    loop {
        let n = device.read_timeout(&mut read_buf, timeout_ms).context("CTAPHID_MSG read")?;
        if n == 0 {
            bail!("CTAPHID_MSG timeout ({timeout_ms}ms)");
        }
        if let Some((cmd, data)) = reassembler.feed(&read_buf[..n])? {
            if cmd != CTAPHID_MSG {
                bail!("Expected CTAPHID_MSG response (0x03), got cmd 0x{cmd:02x}");
            }
            return Ok(data);
        }
    }
}
