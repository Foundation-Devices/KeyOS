// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundationdevices.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use {
    crate::dma::{
        DmaChunkSize,
        DmaDataWidth,
        DmaPeripheralId,
        DmaPeripheralTransferConfig,
        DmaTransferDirection,
    },
    utralib::{utra::aes::*, CSR},
};

/// The AES peripheral.
pub struct Aes {
    base_addr: usize,
}

impl Default for Aes {
    fn default() -> Self {
        Self {
            base_addr: utralib::HW_AES_BASE,
        }
    }
}

const BLOCK_SIZE: usize = 16;

const XTS_POLYNOMIAL: u128 = 0x87;

pub const IDATAR_OFFSET: u32 = 0x40;
pub const ODATAR_OFFSET: u32 = 0x50;

#[derive(Debug)]
enum OpModeValue {
    /// ECB: Electronic Codebook mode
    Ecb = 0,
    /// CBC: Cipher Block Chaining mode
    Cbc = 1,
    /// OFB: Output Feedback mode
    #[allow(dead_code)]
    Ofb = 2,
    /// CFB: Cipher Feedback mode
    #[allow(dead_code)]
    Cfb = 3,
    /// CTR: Counter mode (16-bit internal counter)
    #[allow(dead_code)]
    Ctr = 4,
    /// GCM: Galois/Counter mode
    #[allow(dead_code)]
    Gcm = 5,
    /// XTS: XEX-based tweaked-codebook mode
    Xts = 6,
}

impl Aes {
    pub const TX_DMA_CONFIG: DmaPeripheralTransferConfig = DmaPeripheralTransferConfig {
        peripheral_id: DmaPeripheralId::AesTx,
        direction: DmaTransferDirection::MemoryToPeripheral,
        data_width: DmaDataWidth::D32,
        chunk_size: DmaChunkSize::C4,
    };
    pub const RX_DMA_CONFIG: DmaPeripheralTransferConfig = DmaPeripheralTransferConfig {
        peripheral_id: DmaPeripheralId::AesRx,
        direction: DmaTransferDirection::PeripheralToMemory,
        data_width: DmaDataWidth::D32,
        chunk_size: DmaChunkSize::C4,
    };
    /// Create AES with a different base address. Useful with virtual memory.
    #[inline]
    pub fn with_alt_base_addr(base_addr: usize) -> Self {
        Self { base_addr }
    }

    /// Initialize the AES peripheral for encryption.
    #[inline]
    pub fn init_encrypt(&mut self, mode: AesMode) {
        self.init_inner(mode, true)
    }

    /// Initialize the AES peripheral for decryption.
    #[inline]
    pub fn init_decrypt(&mut self, mode: AesMode) {
        self.init_inner(mode, false)
    }

    fn init_inner(&self, mode: AesMode, encrypt: bool) {
        self.reset();

        let mut csr = CSR::new(self.base_addr as *mut u32);
        let (opmod, key_len) = match &mode {
            AesMode::Ecb { key } => (OpModeValue::Ecb, key.0.len()),
            AesMode::Cbc { key, .. } => (OpModeValue::Cbc, key.0.len()),
            AesMode::Gcm { key, .. } => (OpModeValue::Gcm, key.0.len()),
            AesMode::Counter { key, .. } => (OpModeValue::Ctr, key.0.len()),
            AesMode::Xts { key1, .. } => (OpModeValue::Xts, key1.0.len()),
        };

        csr.rmwf(MR_OPMOD, opmod as u32);
        csr.rmwf(MR_CIPHER, if encrypt { 1 } else { 0 });
        let keysize = match key_len {
            16 => 0,
            24 => 1,
            _ => 2,
        };
        csr.rmwf(MR_KEYSIZE, keysize);

        match mode {
            AesMode::Ecb { key } => {
                self.set_key(&key);
            }
            AesMode::Cbc { key, iv } => {
                self.set_key(&key);
                self.set_iv(&iv);
            }
            AesMode::Gcm {
                key,
                iv,
                ctr,
                ghash,
            } => {
                self.set_key(&key);
                // In GCM mode, setting the key will also set H and GHASH, but that's not instant.
                while !self.is_data_ready() {}
                self.set_iv(&Iv::from_gcm_params(iv, ctr));
                self.set_ghash(&ghash);
                self.set_gcm_aadlen(0);
            }
            AesMode::Counter { key, ctr_value } => {
                self.set_key(&key);
                self.set_iv(&ctr_value);
            }
            AesMode::Xts {
                key1,
                key2,
                tweak,
                j,
            } => {
                // Temporarily switch to ECB to encrypt the tweak value with key2
                let mut sub_aes = Self {
                    base_addr: self.base_addr,
                };
                sub_aes.init_encrypt(AesMode::Ecb { key: key2 });
                let mut encrypted_tweak = [0u8; 16];
                sub_aes.process(&tweak, &mut encrypted_tweak);

                // Switch back to the XTS mode and select encryption or decryption
                csr.rmwf(MR_OPMOD, OpModeValue::Xts as u32);
                csr.rmwf(MR_CIPHER, if encrypt { 1 } else { 0 });
                csr.rmwf(MR_KEYSIZE, keysize);

                // AES_TWRx must be written with the encrypted Tweak Value
                // with bytes swapped as described in AES Register Endianness.
                encrypted_tweak.reverse();
                self.set_tweak(&encrypted_tweak);

                // Set the alpha primitive corresponding to the first block of the sector
                self.set_alpha(&Self::compute_alpha(j));

                // Set key1 as the main key
                self.set_key(&key1);
            }
        }
    }

    fn compute_alpha(j: usize) -> [u32; 4] {
        let mut alpha = 1u128;

        // Multiply j times with 2, over GF128 using the XTS_POLYNOMIAL
        for _ in 0..j {
            alpha = (alpha << 1)
                ^ if alpha & (1 << 127) != 0 {
                    XTS_POLYNOMIAL
                } else {
                    0
                };
        }
        [
            alpha as u32,
            (alpha >> 32) as u32,
            (alpha >> 64) as u32,
            (alpha >> 96) as u32,
        ]
    }

    #[inline]
    pub fn process(&self, input: &[u8], output: &mut [u8]) {
        self.set_auto_start();

        for (in_block, out_block) in input
            .chunks_exact(BLOCK_SIZE)
            .zip(output.chunks_exact_mut(BLOCK_SIZE))
        {
            self.set_input_data(in_block);

            while !self.is_data_ready() {
                // Wait for data ready
            }

            self.read_output_data(out_block);
        }
    }

    #[inline]
    pub fn process_aad(&self, input: &[u8]) {
        self.set_auto_start();
        self.set_gcm_aadlen(input.len() as u32);

        for in_block in input.chunks(BLOCK_SIZE) {
            if in_block.len() == BLOCK_SIZE {
                self.set_input_data(in_block);
            } else {
                let mut block = [0; BLOCK_SIZE];
                block[..in_block.len()].copy_from_slice(in_block);
                self.set_input_data(&block);
            }
            while !self.is_data_ready() {}
        }
    }

    #[inline]
    pub fn dma_tx_addr(&self) -> usize {
        self.base_addr + IDATAR_OFFSET as usize
    }

    #[inline]
    pub fn dma_rx_addr(&self) -> usize {
        self.base_addr + ODATAR_OFFSET as usize
    }

    fn set_input_data(&self, block: &[u8]) {
        let idatar_base = self.base_addr + IDATAR_OFFSET as usize;

        for (i, word) in block.chunks_exact(4).enumerate() {
            unsafe {
                let ptr = (idatar_base + i * 4) as *mut u32;
                let word = u32::from_le_bytes(word.try_into().unwrap());
                ptr.write_volatile(word);
            }
        }
    }

    fn read_output_data(&self, output: &mut [u8]) {
        let odatar_base = self.base_addr + ODATAR_OFFSET as usize;
        for (i, word) in output.chunks_exact_mut(4).enumerate() {
            unsafe {
                let ptr = (odatar_base + i * 4) as *const u32;
                let word_u32 = ptr.read_volatile();
                word.copy_from_slice(&word_u32.to_le_bytes());
            }
        }
    }

    fn set_key(&self, key: &Key) {
        const AES_KEYWR_OFFSET: usize = 0x20;
        let keywr_base = self.base_addr + AES_KEYWR_OFFSET;

        for (i, key) in key.0.chunks(4).enumerate() {
            unsafe {
                let ptr = (keywr_base + i * 4) as *mut u32;
                ptr.write_volatile(u32::from_le_bytes(key.try_into().unwrap()));
            }
        }
    }

    fn set_iv(&self, iv: &Iv) {
        const AES_IVR_OFFSET: usize = 0x60;
        let ivr_base = self.base_addr + AES_IVR_OFFSET;

        for (i, iv) in iv.0.iter().enumerate() {
            unsafe {
                let ptr = (ivr_base + i * 4) as *mut u32;
                ptr.write_volatile(*iv);
            }
        }
    }

    fn set_alpha(&self, alpha: &[u32; 4]) {
        const AES_ALPHAR_OFFSET: usize = 0xD0;
        let alphar_offset = self.base_addr + AES_ALPHAR_OFFSET;

        for (i, alpha) in alpha.iter().enumerate() {
            unsafe {
                let ptr = (alphar_offset + i * 4) as *mut u32;
                ptr.write_volatile(*alpha);
            }
        }
    }

    fn set_tweak(&self, tweak: &[u8; 16]) {
        const TWR_OFFSET: usize = 0xC0;
        let twr_base = self.base_addr + TWR_OFFSET;

        for (i, word) in tweak.chunks_exact(4).enumerate() {
            unsafe {
                let ptr = (twr_base + i * 4) as *mut u32;
                let word = u32::from_le_bytes(word.try_into().unwrap());
                ptr.write_volatile(word);
            }
        }
    }

    fn set_gcm_aadlen(&self, len: u32) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wo(AADLENR, len);
    }

    pub fn get_ghash(&self) -> [u32; 4] {
        const GHASH_OFFSET: usize = 0x78;
        let mut result = [0; 4];
        let ghash_base = self.base_addr + GHASH_OFFSET;

        for (i, word) in result.iter_mut().enumerate() {
            unsafe {
                let ptr = (ghash_base + i * 4) as *mut u32;
                *word = ptr.read_volatile();
            }
        }
        result
    }

    fn set_ghash(&self, ghash: &[u32; 4]) {
        const GHASH_OFFSET: usize = 0x78;
        let ghash_base = self.base_addr + GHASH_OFFSET;

        for (i, word) in ghash.iter().enumerate() {
            unsafe {
                let ptr = (ghash_base + i * 4) as *mut u32;
                ptr.write_volatile(*word);
            }
        }
    }

    #[inline]
    pub fn setup_for_dma(&self, len: usize) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(MR_SMOD, 2); // DMA auto-start
        csr.rmwf(MR_DUALBUFF, 1); // Dual-buffering to increase performance
        csr.wo(CLENR, len as u32); // GCM Plaintext/Ciphertext length
    }

    fn set_auto_start(&self) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(MR_SMOD, 1);
    }

    fn is_data_ready(&self) -> bool {
        let csr = CSR::new(self.base_addr as *mut u32);
        csr.rf(ISR_DATRDY) != 0
    }

    fn reset(&self) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(CR_SWRST, 1);
        csr.wfo(MR_CKEY, CKEY);
    }
}

const CKEY: u32 = 0xE;

#[derive(Debug, Clone)]
pub enum AesMode<'a> {
    Ecb {
        key: Key<'a>,
    },

    Cbc {
        key: Key<'a>,
        iv: Iv,
    },

    Gcm {
        key: Key<'a>,
        iv: [u32; 3],
        ctr: u32,
        ghash: [u32; 4],
    },

    Counter {
        key: Key<'a>,
        ctr_value: Iv,
    },

    Xts {
        /// Block encryption key
        key1: Key<'a>,
        /// Tweak encryption key
        key2: Key<'a>,
        /// Tweak value (spans multiple AES blocks)
        tweak: [u8; 16],
        /// Block offset value within a single tweak
        j: usize,
    },
}

#[derive(Clone)]
pub struct Key<'a>(&'a [u8]);

#[derive(Debug, Clone, Copy)]
pub struct KeyIsWrongSize;

impl<'a> TryFrom<&'a [u8]> for Key<'a> {
    type Error = KeyIsWrongSize;

    fn try_from(value: &'a [u8]) -> Result<Self, Self::Error> {
        match value.len() {
            16 | 24 | 32 => Ok(Self(value)),
            _ => Err(KeyIsWrongSize),
        }
    }
}

impl<'a> From<&'a [u8; 32]> for Key<'a> {
    fn from(value: &'a [u8; 32]) -> Self {
        Self(value as &[u8])
    }
}

impl<'a> core::fmt::Debug for Key<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Key{}", self.0.len() * 8)
    }
}

impl<'a> Key<'a> {
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|&b| b == 0)
    }
}

#[derive(Debug, Default, Clone)]
pub struct Iv([u32; 4]);

impl Iv {
    #[inline]
    pub fn try_from_slice(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != BLOCK_SIZE {
            return None;
        }

        let mut iv = [0; 4];
        for (i, chunk) in bytes.chunks_exact(4).enumerate() {
            iv[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        Some(Self(iv))
    }

    #[inline]
    pub fn from_gcm_params(iv: [u32; 3], ctr: u32) -> Self {
        Self([iv[0], iv[1], iv[2], ctr.to_be()])
    }

    #[inline]
    pub fn add(&mut self, aes_blocks: usize) {
        let mut to_add = aes_blocks as u32;
        for part in &mut self.0 {
            let (new, overflow) = part.overflowing_add(to_add);
            *part = new;
            if overflow {
                to_add = 1
            } else {
                break;
            }
        }
    }

    // See SAMA5D2 datasheet 60.4.2:
    // "In CTR mode, the size of the block counter embedded in the module is 16 bits.
    // Therefore, there is a rollover after processing 1 Mbyte of data. If the file to be
    // processed is greater than 1 Mbyte, this file must be split into fragments of 1 Mbyte or
    // less for the first fragment if the initial value of the counter is greater than 0."
    #[inline]
    pub fn is_ctr_rollover(&self, blocks: usize) -> bool {
        (self.0[3].to_be() & 0xFFFF) + blocks as u32 >= 0x10000
    }
}
