// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{self, Read, Seek, Write};

#[cfg(feature = "build")]
use sha2::Digest;

#[cfg(feature = "build")]
use crate::{Header, Version};

/// Minimum exact-match length. Shorter matches fragment the patch into more
/// control records, and 48 measures smaller on real firmware than either
/// direction from it (SFT-7121).
#[cfg(feature = "build")]
const SMALL_MATCH: usize = 48;

/// Target the patcher plans in one go, which is also the span its source reads
/// are sorted within. A wider span leaves smaller gaps between neighbouring
/// reads, so more of them merge: on the real app.bin an apply takes 19.3 s at
/// 256KB, 16.1 at 1MB, 13.7 at 2MB and 12.5 at 4MB. It is held to 2MB because
/// it competes with the decode windows and the compressed patch body, and the
/// device is already close to its memory threshold during an apply.
const PATCH_BUFFER: usize = 2 * 1024 * 1024;

/// How a patch body is wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Bzip2 around a bzip2-compressed bsdiff patch. Only [`build`] understands
    /// it, so that firmware too old to read [`Format::Zstd`] can still be
    /// patched forward.
    LegacyBzip2,
    /// A zstd-compressed bsdiff patch, stored as-is. Compressing it again buys
    /// nothing.
    Zstd,
}

impl Format {
    /// Bytes [`Format::detect`] needs to see.
    pub const MAGIC_LEN: usize = 8;

    /// The format a patch body starts with, or `None` if it is neither.
    pub fn detect(body: &[u8]) -> Option<Self> {
        if body.starts_with(b"BSDIFF4Z") {
            Some(Format::Zstd)
        } else if body.starts_with(b"BZh") {
            Some(Format::LegacyBzip2)
        } else {
            None
        }
    }
}

/// Write a patch turning `old` into `new`, header first.
#[cfg(feature = "build")]
pub fn build<W: Write>(
    old: &[u8],
    old_version: Version,
    new: &[u8],
    new_version: Version,
    format: Format,
    mut out: W,
) -> io::Result<()> {
    let header = Header {
        old_version,
        old_file_size: old.len() as u64,
        old_file_hash: sha2::Sha256::digest(old).into(),
        new_version,
        new_file_size: new.len() as u64,
        new_file_hash: sha2::Sha256::digest(new).into(),
    };
    header.write_to(&mut out)?;

    let bsdiff = qbsdiff::Bsdiff::new(old, new).small_match(SMALL_MATCH);
    match format {
        Format::LegacyBzip2 => {
            let body = bzip2::write::BzEncoder::new(out, bzip2::Compression::best());
            bsdiff.codec(qbsdiff::Codec::Bzip2).compare(body)?;
        }
        Format::Zstd => {
            bsdiff.codec(qbsdiff::Codec::Zstd).compare(out)?;
        }
    }
    Ok(())
}

/// apply a patch through three independent readers positioned at its body
pub fn apply<B, S, T>(
    bodies: [B; 3],
    body_size: u64,
    source: S,
    scratch: &mut [u8],
    target: T,
) -> io::Result<u64>
where
    B: Read + Seek,
    S: Read + Seek,
    T: Write,
{
    qbsdiff::Bspatch::from_readers(bodies, body_size)?
        .buffer_size(PATCH_BUFFER)
        .apply(source, scratch, target)
}

#[cfg(all(test, feature = "build"))]
mod tests {
    use super::*;

    fn sample() -> (Vec<u8>, Vec<u8>) {
        (
            b"the quick brown fox jumps over the lazy dog".repeat(64),
            b"the quick brown cat jumps over the lazy dog".repeat(64),
        )
    }

    fn build_patch(format: Format) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (old, new) = sample();
        let mut patch = Vec::new();
        build(
            &old,
            Version::parse("v1.3.1").unwrap(),
            &new,
            Version::parse("v1.4.0").unwrap(),
            format,
            &mut patch,
        )
        .unwrap();
        (old, new, patch)
    }

    #[test]
    fn zstd_round_trips() {
        let (old, new, patch) = build_patch(Format::Zstd);

        let mut body = patch.as_slice();
        let header = Header::read_from(&mut body).unwrap();
        assert_eq!(header.old_version, Version::parse("v1.3.1").unwrap());
        assert_eq!(header.new_file_size, new.len() as u64);
        assert_eq!(header.new_file_hash, <[u8; 32]>::from(sha2::Sha256::digest(&new)));
        assert_eq!(&body[..8], b"BSDIFF4Z");

        let bodies = [io::Cursor::new(body), io::Cursor::new(body), io::Cursor::new(body)];
        let mut patched = Vec::new();
        apply(bodies, body.len() as u64, io::Cursor::new(&old), &mut [0; 4096], &mut patched).unwrap();
        assert_eq!(patched, new);
    }

    #[test]
    fn legacy_is_still_double_bzip2() {
        let (_, _, patch) = build_patch(Format::LegacyBzip2);
        let mut body = patch.as_slice();
        Header::read_from(&mut body).unwrap();
        assert_eq!(&body[..3], b"BZh");
    }

    #[test]
    fn apply_rejects_a_legacy_body() {
        let (old, _, patch) = build_patch(Format::LegacyBzip2);
        let mut body = patch.as_slice();
        Header::read_from(&mut body).unwrap();

        let bodies = [io::Cursor::new(body), io::Cursor::new(body), io::Cursor::new(body)];
        let err =
            apply(bodies, body.len() as u64, io::Cursor::new(&old), &mut [0; 4096], Vec::new()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
