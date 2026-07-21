// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Flux HID transport framing.
//!
//! Frames/deframes ISO 7816 APDUs over 64-byte USB HID reports using the
//! HID protocol (2-byte channel ID, tag `0x05`, 2-byte sequence number).

/// HID report size in bytes.
pub const REPORT_SIZE: usize = 64;

/// APDU tag byte.
pub const TAG_APDU: u8 = 0x05;

/// Header length for an initialization packet (channel:2 + tag:1 + seq:2 + len:2).
pub const INIT_HEADER_LEN: usize = 7;

/// Header length for a continuation packet (channel:2 + tag:1 + seq:2).
pub const CONT_HEADER_LEN: usize = 5;

/// Data capacity of an initialization packet.
pub const INIT_DATA_CAPACITY: usize = REPORT_SIZE - INIT_HEADER_LEN; // 57

/// Data capacity of a continuation packet.
pub const CONT_DATA_CAPACITY: usize = REPORT_SIZE - CONT_HEADER_LEN; // 59

/// Maximum APDU payload length (limited by the 2-byte length field).
const MAX_APDU_LEN: usize = u16::MAX as usize;

/// Errors that can occur during HID transport framing.
#[derive(Debug, thiserror::Error)]
pub enum HidError {
    #[error("Report too short: {0} bytes")]
    ReportTooShort(usize),

    #[error("Invalid tag: expected 0x05, got 0x{0:02x}")]
    InvalidTag(u8),

    #[error("Sequence mismatch: expected {expected}, got {actual}")]
    SequenceMismatch { expected: u16, actual: u16 },

    #[error("Channel mismatch: expected 0x{expected:04x}, got 0x{actual:04x}")]
    ChannelMismatch { expected: u16, actual: u16 },

    #[error("Init packet too short: {0} bytes")]
    InitTooShort(usize),

    #[error("Payload too large: {0} bytes")]
    PayloadTooLarge(usize),
}

/// Reassembles incoming HID reports into complete APDUs.
pub struct Reassembler {
    buf: Vec<u8>,
    remaining: usize,
    expected_seq: u16,
    channel_id: Option<u16>,
}

impl Reassembler {
    /// Creates a new reassembler with no state.
    pub fn new() -> Self { Self { buf: Vec::new(), remaining: 0, expected_seq: 0, channel_id: None } }

    /// Feed a raw HID report into the reassembler.
    ///
    /// Returns `Ok(Some((channel_id, apdu)))` when a complete APDU has been
    /// reassembled, `Ok(None)` when more packets are needed, or an error if the
    /// report is malformed.
    pub fn feed(&mut self, report: &[u8]) -> Result<Option<(u16, Vec<u8>)>, HidError> {
        if report.len() < CONT_HEADER_LEN {
            return Err(HidError::ReportTooShort(report.len()));
        }

        let channel_id = u16::from_be_bytes([report[0], report[1]]);
        let tag = report[2];
        let seq = u16::from_be_bytes([report[3], report[4]]);

        if tag != TAG_APDU {
            return Err(HidError::InvalidTag(tag));
        }

        if seq == 0 {
            // Initialization packet
            if report.len() < INIT_HEADER_LEN {
                return Err(HidError::InitTooShort(report.len()));
            }

            let total_len = u16::from_be_bytes([report[5], report[6]]) as usize;

            self.buf.clear();
            self.buf.reserve(total_len);
            self.channel_id = Some(channel_id);

            let available = (report.len() - INIT_HEADER_LEN).min(INIT_DATA_CAPACITY);
            let to_copy = available.min(total_len);
            self.buf.extend_from_slice(&report[INIT_HEADER_LEN..INIT_HEADER_LEN + to_copy]);
            self.remaining = total_len.saturating_sub(to_copy);
            self.expected_seq = 1;
        } else {
            // Continuation packet
            if seq != self.expected_seq {
                return Err(HidError::SequenceMismatch { expected: self.expected_seq, actual: seq });
            }
            if let Some(expected) = self.channel_id {
                if channel_id != expected {
                    return Err(HidError::ChannelMismatch { expected, actual: channel_id });
                }
            }

            let available = (report.len() - CONT_HEADER_LEN).min(CONT_DATA_CAPACITY);
            let to_copy = available.min(self.remaining);
            self.buf.extend_from_slice(&report[CONT_HEADER_LEN..CONT_HEADER_LEN + to_copy]);
            self.remaining = self.remaining.saturating_sub(to_copy);
            self.expected_seq += 1;
        }

        if self.remaining == 0 && !self.buf.is_empty() {
            let apdu = core::mem::take(&mut self.buf);
            let cid = self.channel_id.unwrap_or(0);
            self.expected_seq = 0;
            self.channel_id = None;
            Ok(Some((cid, apdu)))
        } else {
            Ok(None)
        }
    }

    /// Clears reassembly state (e.g. on timeout or disconnect).
    pub fn reset(&mut self) {
        self.buf.clear();
        self.remaining = 0;
        self.expected_seq = 0;
        self.channel_id = None;
    }

    /// Returns `true` if reassembly of a multi-packet APDU is in progress.
    #[allow(dead_code)]
    pub fn in_progress(&self) -> bool { self.remaining > 0 }
}

/// Fragments an APDU into 64-byte HID reports for transmission.
///
/// Returns a vector of fixed-size reports, zero-padded as required by the
/// HID protocol.
pub fn fragment(channel_id: u16, apdu: &[u8]) -> Result<Vec<[u8; REPORT_SIZE]>, HidError> {
    if apdu.len() > MAX_APDU_LEN {
        return Err(HidError::PayloadTooLarge(apdu.len()));
    }

    let total_len = apdu.len();
    let mut reports = Vec::new();
    let mut offset = 0;
    let mut seq: u16 = 0;

    // Initialization report
    let mut report = [0u8; REPORT_SIZE];
    report[0..2].copy_from_slice(&channel_id.to_be_bytes());
    report[2] = TAG_APDU;
    report[3..5].copy_from_slice(&0u16.to_be_bytes());
    report[5..7].copy_from_slice(&(total_len as u16).to_be_bytes());
    let chunk = total_len.min(INIT_DATA_CAPACITY);
    report[INIT_HEADER_LEN..INIT_HEADER_LEN + chunk].copy_from_slice(&apdu[..chunk]);
    offset += chunk;
    reports.push(report);
    seq += 1;

    // Continuation reports
    while offset < total_len {
        let mut report = [0u8; REPORT_SIZE];
        report[0..2].copy_from_slice(&channel_id.to_be_bytes());
        report[2] = TAG_APDU;
        report[3..5].copy_from_slice(&seq.to_be_bytes());
        let chunk = (total_len - offset).min(CONT_DATA_CAPACITY);
        report[CONT_HEADER_LEN..CONT_HEADER_LEN + chunk].copy_from_slice(&apdu[offset..offset + chunk]);
        offset += chunk;
        reports.push(report);
        seq += 1;
    }

    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Reassembly tests --

    #[test]
    fn reassemble_single_packet_apdu() {
        let apdu = [0xe0, 0x06, 0x00, 0x00, 0x00]; // GET_APP_CONFIGURATION
        let mut report = [0u8; REPORT_SIZE];
        report[0..2].copy_from_slice(&0xa502u16.to_be_bytes());
        report[2] = TAG_APDU;
        report[3..5].copy_from_slice(&0u16.to_be_bytes());
        report[5..7].copy_from_slice(&(apdu.len() as u16).to_be_bytes());
        report[7..7 + apdu.len()].copy_from_slice(&apdu);

        let mut r = Reassembler::new();
        let result = r.feed(&report).unwrap();
        let (cid, reassembled) = result.unwrap();
        assert_eq!(cid, 0xa502);
        assert_eq!(reassembled, apdu);
    }

    #[test]
    fn reassemble_multi_packet_apdu() {
        // 141-byte APDU (from the HID wire spec GET_PUBLIC_KEY response example)
        let apdu: Vec<u8> = (0u8..141).collect();
        let channel_id: u16 = 0xbe02;

        let reports = fragment(channel_id, &apdu).unwrap();
        assert_eq!(reports.len(), 3); // 57 + 59 + 25 = 141

        let mut r = Reassembler::new();
        assert!(r.feed(&reports[0]).unwrap().is_none());
        assert!(r.in_progress());
        assert!(r.feed(&reports[1]).unwrap().is_none());
        assert!(r.in_progress());
        let result = r.feed(&reports[2]).unwrap();
        let (cid, reassembled) = result.unwrap();
        assert_eq!(cid, channel_id);
        assert_eq!(reassembled, apdu);
        assert!(!r.in_progress());
    }

    #[test]
    fn reassemble_sequence_mismatch() {
        let apdu: Vec<u8> = vec![0; 120]; // needs 2 packets
        let reports = fragment(0x0101, &apdu).unwrap();

        let mut r = Reassembler::new();
        assert!(r.feed(&reports[0]).unwrap().is_none());

        // Corrupt the sequence number in the continuation packet
        let mut bad_report = reports[1];
        bad_report[3..5].copy_from_slice(&5u16.to_be_bytes()); // wrong seq

        let err = r.feed(&bad_report).unwrap_err();
        assert!(matches!(err, HidError::SequenceMismatch { expected: 1, actual: 5 }));
    }

    #[test]
    fn reassemble_channel_mismatch() {
        let apdu: Vec<u8> = vec![0; 120]; // needs 2 packets
        let reports = fragment(0x0101, &apdu).unwrap();

        let mut r = Reassembler::new();
        assert!(r.feed(&reports[0]).unwrap().is_none());

        let mut bad_report = reports[1];
        bad_report[0..2].copy_from_slice(&0x0202u16.to_be_bytes());

        let err = r.feed(&bad_report).unwrap_err();
        assert!(matches!(err, HidError::ChannelMismatch { expected: 0x0101, actual: 0x0202 }));
    }

    #[test]
    fn reassemble_invalid_tag() {
        let mut report = [0u8; REPORT_SIZE];
        report[0..2].copy_from_slice(&0xa502u16.to_be_bytes());
        report[2] = 0x99; // bad tag

        let mut r = Reassembler::new();
        let err = r.feed(&report).unwrap_err();
        assert!(matches!(err, HidError::InvalidTag(0x99)));
    }

    #[test]
    fn reassemble_report_too_short() {
        let mut r = Reassembler::new();
        let err = r.feed(&[0x00, 0x01]).unwrap_err();
        assert!(matches!(err, HidError::ReportTooShort(2)));
    }

    #[test]
    fn reassemble_reset_mid_reassembly() {
        let apdu: Vec<u8> = vec![0xAA; 120];
        let reports = fragment(0x0101, &apdu).unwrap();

        let mut r = Reassembler::new();
        assert!(r.feed(&reports[0]).unwrap().is_none());
        assert!(r.in_progress());

        r.reset();
        assert!(!r.in_progress());

        // Can start a fresh reassembly after reset
        let result = r.feed(&reports[0]).unwrap();
        assert!(result.is_none()); // still needs more packets
    }

    #[test]
    fn reassemble_max_payload_len_accepted() {
        let mut report = [0u8; REPORT_SIZE];
        report[0..2].copy_from_slice(&0x0101u16.to_be_bytes());
        report[2] = TAG_APDU;
        report[3..5].copy_from_slice(&0u16.to_be_bytes());
        report[5..7].copy_from_slice(&u16::MAX.to_be_bytes());

        let mut r = Reassembler::new();
        assert!(r.feed(&report).unwrap().is_none());
        assert!(r.in_progress());
    }

    // -- Fragmentation tests --

    #[test]
    fn fragment_single_report() {
        let apdu = [0xe0, 0x06, 0x00, 0x00, 0x00];
        let reports = fragment(0xa502, &apdu).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0][0..2], 0xa502u16.to_be_bytes());
        assert_eq!(reports[0][2], TAG_APDU);
        assert_eq!(reports[0][3..5], 0u16.to_be_bytes());
        assert_eq!(reports[0][5..7], (apdu.len() as u16).to_be_bytes());
        assert_eq!(&reports[0][7..12], &apdu);
        // Remaining bytes should be zero (padding)
        assert!(reports[0][12..].iter().all(|&b| b == 0));
    }

    #[test]
    fn fragment_three_reports_141_bytes() {
        let apdu: Vec<u8> = (0u8..141).collect();
        let reports = fragment(0xbe02, &apdu).unwrap();
        assert_eq!(reports.len(), 3);

        // Report 1: init, seq=0
        assert_eq!(reports[0][0..2], 0xbe02u16.to_be_bytes());
        assert_eq!(reports[0][2], TAG_APDU);
        assert_eq!(reports[0][3..5], 0u16.to_be_bytes());
        assert_eq!(reports[0][5..7], 141u16.to_be_bytes());
        assert_eq!(&reports[0][7..], &apdu[..57]);

        // Report 2: continuation, seq=1
        assert_eq!(reports[1][0..2], 0xbe02u16.to_be_bytes());
        assert_eq!(reports[1][2], TAG_APDU);
        assert_eq!(reports[1][3..5], 1u16.to_be_bytes());
        assert_eq!(&reports[1][5..], &apdu[57..116]);

        // Report 3: continuation, seq=2, with padding
        assert_eq!(reports[2][0..2], 0xbe02u16.to_be_bytes());
        assert_eq!(reports[2][2], TAG_APDU);
        assert_eq!(reports[2][3..5], 2u16.to_be_bytes());
        assert_eq!(&reports[2][5..30], &apdu[116..141]);
        // 34 bytes of zero padding
        assert!(reports[2][30..].iter().all(|&b| b == 0));
    }

    #[test]
    fn fragment_oversized_apdu_rejected() {
        let apdu = vec![0u8; MAX_APDU_LEN + 1];
        let err = fragment(0x0101, &apdu).unwrap_err();
        assert!(matches!(err, HidError::PayloadTooLarge(_)));
    }

    // -- Round-trip tests --

    #[test]
    fn round_trip_fragment_then_reassemble() {
        for size in [1, 5, 57, 58, 116, 141, 175, 1000] {
            let apdu: Vec<u8> = (0..size).map(|i: usize| (i % 256) as u8).collect();
            let channel_id: u16 = 0x1234;

            let reports = fragment(channel_id, &apdu).unwrap();

            let mut r = Reassembler::new();
            let mut result = None;
            for report in &reports {
                match r.feed(report).unwrap() {
                    Some(r) => {
                        result = Some(r);
                        break;
                    }
                    None => {}
                }
            }

            let (cid, reassembled) =
                result.unwrap_or_else(|| panic!("Failed to reassemble {size}-byte APDU"));
            assert_eq!(cid, channel_id);
            assert_eq!(reassembled, apdu, "Round-trip failed for {size}-byte APDU");
        }
    }

    // -- Wire example tests (from the HID wire spec) --

    #[test]
    fn wire_example_get_app_configuration_request() {
        // From the HID wire spec: a5 02 05 00 00 00 05 e0 06 00 00 00 + zero padding
        let mut report = [0u8; REPORT_SIZE];
        report[..12]
            .copy_from_slice(&[0xa5, 0x02, 0x05, 0x00, 0x00, 0x00, 0x05, 0xe0, 0x06, 0x00, 0x00, 0x00]);

        let mut r = Reassembler::new();
        let (cid, apdu) = r.feed(&report).unwrap().unwrap();
        assert_eq!(cid, 0xa502);
        assert_eq!(apdu, [0xe0, 0x06, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn wire_example_get_app_configuration_response() {
        // From the HID wire spec: a5 02 05 00 00 00 06 02 01 0f 00 90 00 + zero padding
        let apdu = [0x02, 0x01, 0x0f, 0x00, 0x90, 0x00];
        let reports = fragment(0xa502, &apdu).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(
            &reports[0][..13],
            &[0xa5, 0x02, 0x05, 0x00, 0x00, 0x00, 0x06, 0x02, 0x01, 0x0f, 0x00, 0x90, 0x00]
        );
    }
}
