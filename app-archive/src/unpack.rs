// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading an archive. The caller keeps its own I/O and its own checks against the signed
//! manifest; all that is needed here is to hand it a bounded reader.

use std::io::Read;

use crate::MAX_BUNDLE_BYTES;

/// Decode an archive, bounded so a small file cannot inflate without end.
///
/// Past the bound the stream ends early and the tar reader reports the truncation, which is the
/// same outcome as any other malformed archive.
pub fn decode<R: Read>(reader: R) -> impl Read { flate2::read::GzDecoder::new(reader).take(MAX_BUNDLE_BYTES) }

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn decode_inflates_a_compressed_archive() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, b"tar bytes").unwrap();
        let compressed = encoder.finish().unwrap();

        let mut bytes = Vec::new();
        decode(Cursor::new(compressed)).read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes, b"tar bytes");
    }

    /// A small archive whose gzip stream inflates without end must not be read into memory.
    #[test]
    fn decode_stops_inflating_at_the_bound() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut encoder, &vec![0u8; 96 * 1024 * 1024]).unwrap();
        let bomb = encoder.finish().unwrap();
        assert!(bomb.len() < 128 * 1024, "the compressed form is far smaller than what it expands to");

        let mut bytes = Vec::new();
        decode(Cursor::new(bomb)).read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes.len() as u64, MAX_BUNDLE_BYTES);
    }
}
