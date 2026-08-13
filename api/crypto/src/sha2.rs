// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
use server::xous::MemoryRange;
use server::{permission_set, CheckedConn};

use crate::error::CryptoError;
use crate::messages::{ShaDrop, ShaGetContext, ShaSetContext, ShaUpdate};
use crate::CryptoApi;

permission_set!(pub trait ShaPermissions { ShaSetContext, ShaUpdate, ShaGetContext, ShaDrop });

pub const SHA224_HASH_SIZE: usize = 28;
pub const SHA256_HASH_SIZE: usize = 32;
pub const SHA384_HASH_SIZE: usize = 48;
pub const SHA512_HASH_SIZE: usize = 64;

#[cfg(keyos)]
const SW_THRESHOLD: usize = 0x1000;

#[cfg(keyos)]
const SCRATCH_CAP: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum ShaAlgo {
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

impl From<usize> for ShaAlgo {
    fn from(value: usize) -> Self {
        match value {
            0 => ShaAlgo::Sha224,
            1 => ShaAlgo::Sha256,
            2 => ShaAlgo::Sha384,
            3 => ShaAlgo::Sha512,
            _ => unreachable!(),
        }
    }
}

impl From<ShaAlgo> for usize {
    fn from(value: ShaAlgo) -> Self {
        match value {
            ShaAlgo::Sha224 => 0,
            ShaAlgo::Sha256 => 1,
            ShaAlgo::Sha384 => 2,
            ShaAlgo::Sha512 => 3,
        }
    }
}

impl ShaAlgo {
    pub fn block_size(self) -> usize {
        match self {
            ShaAlgo::Sha224 | ShaAlgo::Sha256 => 64,
            ShaAlgo::Sha384 | ShaAlgo::Sha512 => 128,
        }
    }

    pub fn hash_size(self) -> usize {
        match self {
            ShaAlgo::Sha224 => SHA224_HASH_SIZE,
            ShaAlgo::Sha256 => SHA256_HASH_SIZE,
            ShaAlgo::Sha384 => SHA384_HASH_SIZE,
            ShaAlgo::Sha512 => SHA512_HASH_SIZE,
        }
    }

    pub fn initial_hash_state(self) -> [u8; 64] {
        match self {
            ShaAlgo::Sha224 => [
                0xc1, 0x05, 0x9e, 0xd8, 0x36, 0x7c, 0xd5, 0x07, 0x30, 0x70, 0xdd, 0x17, 0xf7, 0x0e, 0x59,
                0x39, 0xff, 0xc0, 0x0b, 0x31, 0x68, 0x58, 0x15, 0x11, 0x64, 0xf9, 0x8f, 0xa7, 0xbe, 0xfa,
                0x4f, 0xa4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0,
            ],
            ShaAlgo::Sha256 => [
                0x6a, 0x09, 0xe6, 0x67, 0xbb, 0x67, 0xae, 0x85, 0x3c, 0x6e, 0xf3, 0x72, 0xa5, 0x4f, 0xf5,
                0x3a, 0x51, 0x0e, 0x52, 0x7f, 0x9b, 0x05, 0x68, 0x8c, 0x1f, 0x83, 0xd9, 0xab, 0x5b, 0xe0,
                0xcd, 0x19, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0,
            ],
            ShaAlgo::Sha384 => [
                0xcb, 0xbb, 0x9d, 0x5d, 0xc1, 0x05, 0x9e, 0xd8, 0x62, 0x9a, 0x29, 0x2a, 0x36, 0x7c, 0xd5,
                0x07, 0x91, 0x59, 0x01, 0x5a, 0x30, 0x70, 0xdd, 0x17, 0x15, 0x2f, 0xec, 0xd8, 0xf7, 0x0e,
                0x59, 0x39, 0x67, 0x33, 0x26, 0x67, 0xff, 0xc0, 0x0b, 0x31, 0x8e, 0xb4, 0x4a, 0x87, 0x68,
                0x58, 0x15, 0x11, 0xdb, 0x0c, 0x2e, 0x0d, 0x64, 0xf9, 0x8f, 0xa7, 0x47, 0xb5, 0x48, 0x1d,
                0xbe, 0xfa, 0x4f, 0xa4,
            ],
            ShaAlgo::Sha512 => [
                0x6a, 0x09, 0xe6, 0x67, 0xf3, 0xbc, 0xc9, 0x08, 0xbb, 0x67, 0xae, 0x85, 0x84, 0xca, 0xa7,
                0x3b, 0x3c, 0x6e, 0xf3, 0x72, 0xfe, 0x94, 0xf8, 0x2b, 0xa5, 0x4f, 0xf5, 0x3a, 0x5f, 0x1d,
                0x36, 0xf1, 0x51, 0x0e, 0x52, 0x7f, 0xad, 0xe6, 0x82, 0xd1, 0x9b, 0x05, 0x68, 0x8c, 0x2b,
                0x3e, 0x6c, 0x1f, 0x1f, 0x83, 0xd9, 0xab, 0xfb, 0x41, 0xbd, 0x6b, 0x5b, 0xe0, 0xcd, 0x19,
                0x13, 0x7e, 0x21, 0x79,
            ],
        }
    }
}

pub struct ShaStreamingContext<P: ShaPermissions> {
    #[cfg_attr(not(keyos), allow(dead_code))]
    conn: CheckedConn<P>,
    algo: ShaAlgo,
    accumulator: Vec<u8>,
    bytes_compressed: u64,
    hash_state: [u8; 64],
    #[cfg(keyos)]
    server_id: Option<usize>,
    #[cfg(keyos)]
    server_authoritative: bool,
    #[cfg(keyos)]
    scratch: Option<xous::DropDeallocate>,
}

pub type Sha256StreamingContext<P> = ShaStreamingContext<P>;

impl<P: ShaPermissions> CryptoApi<P> {
    pub fn sha2(&self, data: &[u8], algo: ShaAlgo) -> Result<Vec<u8>, CryptoError> {
        let mut ctx = self.sha_init(algo);
        ctx.update(data)?;
        ctx.finalize()
    }

    pub fn sha224(&self, data: &[u8]) -> Result<[u8; SHA224_HASH_SIZE], CryptoError> {
        Ok(self.sha2(data, ShaAlgo::Sha224)?.try_into().unwrap())
    }

    pub fn sha256(&self, data: &[u8]) -> Result<[u8; SHA256_HASH_SIZE], CryptoError> {
        Ok(self.sha2(data, ShaAlgo::Sha256)?.try_into().unwrap())
    }

    pub fn sha384(&self, data: &[u8]) -> Result<[u8; SHA384_HASH_SIZE], CryptoError> {
        Ok(self.sha2(data, ShaAlgo::Sha384)?.try_into().unwrap())
    }

    pub fn sha512(&self, data: &[u8]) -> Result<[u8; SHA512_HASH_SIZE], CryptoError> {
        Ok(self.sha2(data, ShaAlgo::Sha512)?.try_into().unwrap())
    }

    pub fn sha_init(&self, algo: ShaAlgo) -> ShaStreamingContext<P> {
        ShaStreamingContext {
            conn: self.conn.clone(),
            algo,
            accumulator: Vec::new(),
            bytes_compressed: 0,
            hash_state: algo.initial_hash_state(),
            #[cfg(keyos)]
            server_id: None,
            #[cfg(keyos)]
            server_authoritative: false,
            #[cfg(keyos)]
            scratch: None,
        }
    }

    pub fn sha256_init(&self) -> ShaStreamingContext<P> { self.sha_init(ShaAlgo::Sha256) }
}

impl<P: ShaPermissions> ShaStreamingContext<P> {
    pub fn hash_size(&self) -> usize { self.algo.hash_size() }

    pub fn update(&mut self, data: &[u8]) -> Result<(), CryptoError> {
        if data.is_empty() {
            return Ok(());
        }
        #[cfg(keyos)]
        if self.accumulator.len() + data.len() >= SW_THRESHOLD {
            self.push_state_to_server()?;
            self.update_hw(data)?;
            return Ok(());
        }
        self.update_sw(data)
    }

    fn update_sw(&mut self, data: &[u8]) -> Result<(), CryptoError> {
        let bs = self.algo.block_size();
        self.accumulator.extend_from_slice(data);
        let num_blocks = self.accumulator.len() / bs;
        if num_blocks > 0 {
            #[cfg(keyos)]
            self.fetch_server_state()?;
            let blocks_end = num_blocks * bs;
            sw_compress_blocks(&mut self.hash_state, self.algo, &self.accumulator[..blocks_end]);
            self.accumulator.drain(..blocks_end);
            self.bytes_compressed += blocks_end as u64;
        }
        Ok(())
    }

    #[cfg_attr(not(keyos), allow(unused_mut))]
    pub fn finalize(mut self) -> Result<Vec<u8>, CryptoError> {
        #[cfg(keyos)]
        self.fetch_server_state()?;

        let total_bits = (self.bytes_compressed + self.accumulator.len() as u64) * 8;
        let pad = sha_padding(&self.accumulator, self.algo, total_bits);
        sw_compress_blocks(&mut self.hash_state, self.algo, &pad);

        Ok(self.hash_state[..self.algo.hash_size()].to_vec())
    }

    #[cfg(keyos)]
    fn push_state_to_server(&mut self) -> Result<(), CryptoError> {
        if !self.server_authoritative {
            let id = self.conn.send_blocking_archive(ShaSetContext {
                context_id: self.server_id,
                algo: self.algo,
                hash_state: self.hash_state,
            })?;
            self.server_id = Some(id);
            self.server_authoritative = true;
        }
        Ok(())
    }

    #[cfg(keyos)]
    fn fetch_server_state(&mut self) -> Result<(), CryptoError> {
        if self.server_authoritative {
            let snap =
                self.conn.send_blocking_archive(ShaGetContext { context_id: self.server_id.unwrap() })?;
            self.hash_state = snap.hash_state;
            self.server_authoritative = false;
        }
        Ok(())
    }

    #[cfg(keyos)]
    fn scratch_range(&mut self) -> Result<MemoryRange, CryptoError> {
        if self.scratch.is_none() {
            let mem = xous::map_memory(None, None, SCRATCH_CAP, xous::MemoryFlags::W)?;
            self.scratch = Some(xous::DropDeallocate::new(mem));
        }
        Ok(**self.scratch.as_ref().unwrap())
    }

    #[cfg(keyos)]
    fn update_hw(&mut self, data: &[u8]) -> Result<(), CryptoError> {
        use xous::keyos::PAGE_SIZE;

        let bs = self.algo.block_size();
        // Fast path: directly lend the page-aligned prefix to HW, accumulate the tail in SW.
        if self.accumulator.is_empty() && (data.as_ptr() as usize) % PAGE_SIZE == 0 && data.len() >= PAGE_SIZE
        {
            let hw_len = (data.len() / PAGE_SIZE) * PAGE_SIZE;
            let mr = unsafe { MemoryRange::new(data.as_ptr() as usize, hw_len)? };
            self.conn.lend_mut(ShaUpdate { context_id: self.server_id.unwrap(), buf: mr, length: hw_len })?;
            self.bytes_compressed += hw_len as u64;
            self.update_sw(&data[hw_len..])?;
            return Ok(());
        }

        let mut scratch = self.scratch_range()?;

        let mut remaining = data;
        while self.accumulator.len() + remaining.len() >= bs {
            let acc_len = self.accumulator.len();
            let from_data = (SCRATCH_CAP - acc_len).min(remaining.len());
            let send_len = (acc_len + from_data) / bs * bs;

            scratch.as_slice_mut::<u8>()[..acc_len].copy_from_slice(&self.accumulator);
            scratch.as_slice_mut::<u8>()[acc_len..send_len].copy_from_slice(&remaining[..send_len - acc_len]);

            remaining = &remaining[send_len - acc_len..];
            self.accumulator.clear();

            self.conn.lend_mut(ShaUpdate {
                context_id: self.server_id.unwrap(),
                buf: scratch.subrange(0, send_len.next_multiple_of(PAGE_SIZE)).unwrap(),
                length: send_len,
            })?;
            self.bytes_compressed += send_len as u64;
        }
        self.accumulator = remaining.to_vec();

        Ok(())
    }
}

impl<P: ShaPermissions> Drop for ShaStreamingContext<P> {
    fn drop(&mut self) {
        #[cfg(keyos)]
        if let Some(id) = self.server_id {
            self.conn.try_send_scalar(ShaDrop(id)).ok();
        }
    }
}

fn sw_compress_blocks(hash_state: &mut [u8; 64], algo: ShaAlgo, data: &[u8]) {
    match algo {
        ShaAlgo::Sha224 | ShaAlgo::Sha256 => {
            let mut state = [0u32; 8];
            for (i, chunk) in hash_state[..32].chunks_exact(4).enumerate() {
                state[i] = u32::from_be_bytes(chunk.try_into().unwrap());
            }
            for block in data.chunks_exact(64) {
                sha2::compress256(&mut state, core::slice::from_ref(block.try_into().unwrap()));
            }
            for (i, w) in state.iter().enumerate() {
                hash_state[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
            }
        }
        ShaAlgo::Sha384 | ShaAlgo::Sha512 => {
            let mut state = [0u64; 8];
            for (i, chunk) in hash_state[..64].chunks_exact(8).enumerate() {
                state[i] = u64::from_be_bytes(chunk.try_into().unwrap());
            }
            for block in data.chunks_exact(128) {
                sha2::compress512(&mut state, core::slice::from_ref(block.try_into().unwrap()));
            }
            for (i, w) in state.iter().enumerate() {
                hash_state[i * 8..i * 8 + 8].copy_from_slice(&w.to_be_bytes());
            }
        }
    }
}

fn sha_padding(acc: &[u8], algo: ShaAlgo, total_bits: u64) -> Vec<u8> {
    let (bs, len_field): (usize, usize) = match algo {
        ShaAlgo::Sha224 | ShaAlgo::Sha256 => (64, 8),
        ShaAlgo::Sha384 | ShaAlgo::Sha512 => (128, 16),
    };

    let mut pad = Vec::with_capacity(bs * 2);
    pad.extend_from_slice(acc);
    pad.push(0x80);

    let len_mod = (pad.len() + len_field) % bs;
    let zeros = if len_mod == 0 { 0 } else { bs - len_mod };
    pad.extend(core::iter::repeat(0u8).take(zeros));

    if len_field == 8 {
        pad.extend_from_slice(&total_bits.to_be_bytes());
    } else {
        pad.extend_from_slice(&(total_bits as u128).to_be_bytes());
    }

    debug_assert_eq!(pad.len() % bs, 0);
    pad
}
