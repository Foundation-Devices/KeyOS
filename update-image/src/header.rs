// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{self, Read, Write};

use crate::Version;

/// Uncompressed header of an updiff patch file, ahead of the compressed body.
///
/// Sizes are little-endian and hashes are SHA256 of the whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub old_version: Version,
    pub old_file_size: u64,
    pub old_file_hash: [u8; 32],
    pub new_version: Version,
    pub new_file_size: u64,
    pub new_file_hash: [u8; 32],
}

impl Header {
    /// Trailing zeroes, so a field can be added without moving the patch body.
    const RESERVED: usize = 128;
    pub const SIZE: usize = 2 * (Version::SIZE + 8 + 32) + Self::RESERVED;

    /// Read the header, leaving the reader on the first byte of the patch body.
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut version = [0; Version::SIZE];
        let mut size = [0; 8];
        let mut hash = [0; 32];

        reader.read_exact(&mut version)?;
        let old_version = Version::from_bytes(version);
        reader.read_exact(&mut size)?;
        let old_file_size = u64::from_le_bytes(size);
        reader.read_exact(&mut hash)?;
        let old_file_hash = hash;

        reader.read_exact(&mut version)?;
        let new_version = Version::from_bytes(version);
        reader.read_exact(&mut size)?;
        let new_file_size = u64::from_le_bytes(size);
        reader.read_exact(&mut hash)?;
        let new_file_hash = hash;

        reader.read_exact(&mut [0; Self::RESERVED])?;

        Ok(Self { old_version, old_file_size, old_file_hash, new_version, new_file_size, new_file_hash })
    }

    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.old_version.to_bytes())?;
        writer.write_all(&self.old_file_size.to_le_bytes())?;
        writer.write_all(&self.old_file_hash)?;
        writer.write_all(&self.new_version.to_bytes())?;
        writer.write_all(&self.new_file_size.to_le_bytes())?;
        writer.write_all(&self.new_file_hash)?;
        writer.write_all(&[0; Self::RESERVED])
    }
}
