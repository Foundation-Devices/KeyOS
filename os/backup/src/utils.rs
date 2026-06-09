// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
//
use std::io::{Read, Seek};

/// read data from a reader in fixed-size chunks
pub struct ChunkedReader<R, const CHUNK_SIZE: usize> {
    reader: R,
    buffer: [u8; CHUNK_SIZE],
}

pub const DEFAULT_CHUNK_SIZE: usize = 65536;
pub type DefaultChunkedReader<R> = ChunkedReader<R, DEFAULT_CHUNK_SIZE>;

impl<R: Read, const CHUNK_SIZE: usize> ChunkedReader<R, CHUNK_SIZE> {
    pub fn new(reader: R) -> Self {
        assert!(CHUNK_SIZE > 0, "CHUNK_SIZE must be greater than 0");
        Self { reader, buffer: [0; CHUNK_SIZE] }
    }

    pub fn next_chunk(&mut self) -> std::io::Result<Option<&[u8]>> {
        let mut total_read = 0;

        loop {
            match self.reader.read(&mut self.buffer[total_read..])? {
                0 => {
                    if total_read == 0 {
                        return Ok(None);
                    } else {
                        // partial (last) chunk
                        return Ok(Some(&self.buffer[..total_read]));
                    }
                }
                n => {
                    total_read += n;
                    if total_read == self.buffer.len() {
                        return Ok(Some(&self.buffer[..]));
                    }
                }
            }
        }
    }
}

pub fn calculate_file_hash<F: Read + Seek>(file: F) -> Result<[u8; 32], std::io::Error> {
    use sha2::{Digest, Sha256};

    let mut file = defer::defer_with(file, |mut file| {
        file.seek(std::io::SeekFrom::Start(0)).ok();
    });

    let mut hasher = Sha256::new();
    file.seek(std::io::SeekFrom::Start(0))?;

    let mut reader = DefaultChunkedReader::new(&mut *file);
    while let Some(chunk) = reader.next_chunk()? {
        hasher.update(chunk);
    }

    let hash = hasher.finalize();

    Ok(hash.into())
}

pub fn hex(data: &[u8]) -> String { data.iter().map(|b| format!("{:02x}", b)).collect::<String>() }

#[cfg(test)]
mod tests {
    use super::*;

    struct ShortReader {
        data: Vec<u8>,
        pos: usize,
        max_read_size: usize,
    }

    impl ShortReader {
        fn new(data: Vec<u8>, max_read_size: usize) -> Self { Self { data, pos: 0, max_read_size } }
    }

    impl Read for ShortReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }

            let remaining = self.data.len() - self.pos;
            let to_read = std::cmp::min(std::cmp::min(buf.len(), remaining), self.max_read_size);

            buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
            self.pos += to_read;
            Ok(to_read)
        }
    }

    #[test]
    fn short_reads_fill_chunks_completely() {
        let data: Vec<u8> = (0u8..100).collect();
        let reader = ShortReader::new(data.clone(), 3);
        let mut chunked = ChunkedReader::<_, 10>::new(reader);

        let mut chunks = Vec::new();
        while let Some(chunk) = chunked.next_chunk().unwrap() {
            chunks.push(chunk.to_vec());
        }

        assert_eq!(chunks.len(), 10);
        assert!(chunks.iter().take(9).all(|c| c.len() == 10));
        assert_eq!(chunks.into_iter().flatten().collect::<Vec<_>>(), data);
    }

    #[test]
    fn partial_last_chunk() {
        let data: Vec<u8> = (0u8..95).collect();
        let reader = ShortReader::new(data.clone(), 3);
        let mut chunked = ChunkedReader::<_, 10>::new(reader);

        let mut chunks = Vec::new();
        while let Some(chunk) = chunked.next_chunk().unwrap() {
            chunks.push(chunk.to_vec());
        }

        assert_eq!(chunks.len(), 10);
        assert_eq!(chunks[9].len(), 5);
        assert_eq!(chunks.into_iter().flatten().collect::<Vec<_>>(), data);
    }
}
