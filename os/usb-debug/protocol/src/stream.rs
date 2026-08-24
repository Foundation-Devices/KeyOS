// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Length-prefixed framing for the simulator debug channel.
//!
//! USB delimits frames with short packets and ZLPs; a stream socket carries no such boundary, so
//! the same frame bodies travel behind a `u32` length prefix instead. Only the delimiter differs:
//! both ends keep encoding `Command` and `Response` exactly as they do over USB.

use std::io::{self, Read, Write};

use crate::MAX_FRAME_LEN;

/// Where the simulator's usb-debug service listens and host tooling connects. Loopback only: this
/// channel injects touches and uploads apps, so it must not be reachable off the machine.
pub const DEFAULT_SIM_ADDR: &str = "127.0.0.1:7664";

/// Overrides [`DEFAULT_SIM_ADDR`] on both ends, so a second simulator can be driven alongside the
/// first.
pub const SIM_ADDR_ENV: &str = "KEYOS_SIM_DEBUG_ADDR";

/// The address both ends resolve, so they cannot disagree about it.
pub fn sim_addr() -> String { std::env::var(SIM_ADDR_ENV).unwrap_or_else(|_| DEFAULT_SIM_ADDR.to_string()) }

/// Write `[LEN:4 LE][header][payload]`. `header` and `payload` stay separate so a screenshot is
/// written straight from the capture buffer.
pub fn write_frame(writer: &mut impl Write, header: &[u8], payload: &[u8]) -> io::Result<()> {
    let len = header.len() + payload.len();
    let len = u32::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("frame of {len} bytes")))?;

    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(header)?;
    writer.write_all(payload)?;
    writer.flush()
}

/// Read one frame body. `Ok(None)` means the peer closed the connection between frames, which is
/// how a client disconnect arrives.
pub fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }

    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit"),
        ));
    }

    let mut frame = vec![0u8; len];
    reader.read_exact(&mut frame)?;
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, FrameType, Status};

    #[test]
    fn frames_round_trip_through_a_stream() {
        let mut wire = Vec::new();
        let mut command = Vec::new();
        Command::InputText("hello".to_string()).encode_into(&mut command);
        write_frame(&mut wire, &command, &[]).unwrap();
        write_frame(&mut wire, &[FrameType::Response as u8, Status::Ok as u8], &[0xaa; 4096]).unwrap();

        let mut wire = wire.as_slice();
        assert_eq!(
            Command::decode(&read_frame(&mut wire).unwrap().unwrap()).unwrap(),
            Command::InputText("hello".to_string())
        );

        let response = read_frame(&mut wire).unwrap().unwrap();
        assert_eq!(response[..2], [FrameType::Response as u8, Status::Ok as u8]);
        assert_eq!(response[2..], [0xaa; 4096]);

        assert!(read_frame(&mut wire).unwrap().is_none());
    }

    #[test]
    fn a_screenshot_sized_frame_fits() {
        let pixels = vec![0u8; 480 * 800 * 4];
        let mut wire = Vec::new();
        write_frame(&mut wire, &[FrameType::Response as u8, Status::Ok as u8], &pixels).unwrap();

        assert_eq!(read_frame(&mut wire.as_slice()).unwrap().unwrap().len(), pixels.len() + 2);
    }

    #[test]
    fn an_oversized_length_is_rejected_before_allocating() {
        let mut wire = u32::MAX.to_le_bytes().to_vec();
        wire.extend_from_slice(b"never read");

        let error = read_frame(&mut wire.as_slice()).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_frame_cut_short_is_an_error_rather_than_a_clean_end() {
        let mut wire = 8u32.to_le_bytes().to_vec();
        wire.extend_from_slice(b"only4");

        assert_eq!(read_frame(&mut wire.as_slice()).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }
}
