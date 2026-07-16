//! SHA hardware accelerator driver.

use {
    crate::dma::{
        DmaChunkSize,
        DmaDataWidth,
        DmaPeripheralId,
        DmaPeripheralTransferConfig,
        DmaTransferDirection,
    },
    bitflags::bitflags,
    utralib::{utra::sha::*, HW_SHA_BASE, *},
};

const HMAC_IPAD: u8 = 0x36;
const HMAC_OPAD: u8 = 0x5c;

const IDATAR_OFFSET: u32 = 0x40;
const IODATAR_OFFSET: u32 = 0x80;

pub const SHA224_HASH_SIZE: usize = 28;
pub const SHA256_HASH_SIZE: usize = 32;
pub const SHA384_HASH_SIZE: usize = 48;
pub const SHA512_HASH_SIZE: usize = 64;

/// Saved SHA hardware context for multi-context support
/// Allows saving and restoring the intermediate hash state so multiple
/// streaming hash operations can be interleaved
#[derive(Clone)]
pub struct ShaHwContext {
    pub algorithm: Algorithm,
    pub total_len: usize,
    pub bytes_remaining: usize,
    pub hash_state: [u8; 64],
}

impl ShaHwContext {
    pub fn new(algorithm: Algorithm, total_len: usize) -> Self {
        Self {
            algorithm,
            total_len,
            bytes_remaining: total_len,
            hash_state: [0u8; 64],
        }
    }
}

pub trait Algo {
    type HashType: HashTypeHelper;
    type ExtHashType: HashTypeHelper;
    type BlockType: HashTypeHelper;
    const BLOCK_SIZE: usize;
    const EMPTY_HASH: Self::HashType;
    const HASH_ALGO: Algorithm;
    const HMAC_ALGO: Algorithm;
}

pub trait HashTypeHelper: Sized {
    fn default() -> Self;
    fn as_slice(&self) -> &[u8];
    fn as_mut_slice(&mut self) -> &mut [u8];
}

impl<const N: usize> HashTypeHelper for [u8; N] {
    fn as_slice(&self) -> &[u8] {
        self.as_slice()
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }

    fn default() -> Self {
        [0; N]
    }
}

pub struct Sha224;

impl Algo for Sha224 {
    type HashType = [u8; SHA224_HASH_SIZE];
    type ExtHashType = [u8; SHA256_HASH_SIZE];
    type BlockType = [u8; Self::BLOCK_SIZE];
    const BLOCK_SIZE: usize = 64;
    const HASH_ALGO: Algorithm = Algorithm::Sha224;
    const HMAC_ALGO: Algorithm = Algorithm::HmacSha224;

    const EMPTY_HASH: Self::HashType = [
        0xd1, 0x4a, 0x02, 0x8c, 0x2a, 0x3a, 0x2b, 0xc9, 0x47, 0x61, 0x02, 0xbb, 0x28, 0x82, 0x34,
        0xc4, 0x15, 0xa2, 0xb0, 0x1f, 0x82, 0x8e, 0xa6, 0x2a, 0xc5, 0xb3, 0xe4, 0x2f,
    ];
}

pub struct Sha256;

impl Algo for Sha256 {
    type HashType = [u8; SHA256_HASH_SIZE];
    type ExtHashType = [u8; SHA256_HASH_SIZE];
    type BlockType = [u8; Self::BLOCK_SIZE];
    const BLOCK_SIZE: usize = 64;
    const HASH_ALGO: Algorithm = Algorithm::Sha256;
    const HMAC_ALGO: Algorithm = Algorithm::HmacSha256;

    const EMPTY_HASH: Self::HashType = [
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
        0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
        0xb8, 0x55,
    ];
}

pub struct Sha384;

impl Algo for Sha384 {
    type HashType = [u8; SHA384_HASH_SIZE];
    type ExtHashType = [u8; SHA512_HASH_SIZE];
    type BlockType = [u8; Self::BLOCK_SIZE];
    const BLOCK_SIZE: usize = 128;
    const HASH_ALGO: Algorithm = Algorithm::Sha384;
    const HMAC_ALGO: Algorithm = Algorithm::HmacSha384;

    const EMPTY_HASH: Self::HashType = [
        0x38, 0xb0, 0x60, 0xa7, 0x51, 0xac, 0x96, 0x38, 0x4c, 0xd9, 0x32, 0x7e, 0xb1, 0xb1, 0xe3,
        0x6a, 0x21, 0xfd, 0xb7, 0x11, 0x14, 0xbe, 0x07, 0x43, 0x4c, 0x0c, 0xc7, 0xbf, 0x63, 0xf6,
        0xe1, 0xda, 0x27, 0x4e, 0xde, 0xbf, 0xe7, 0x6f, 0x65, 0xfb, 0xd5, 0x1a, 0xd2, 0xf1, 0x48,
        0x98, 0xb9, 0x5b,
    ];
}

pub struct Sha512;

impl Algo for Sha512 {
    type HashType = [u8; SHA512_HASH_SIZE];
    type ExtHashType = [u8; SHA512_HASH_SIZE];
    type BlockType = [u8; Self::BLOCK_SIZE];
    const BLOCK_SIZE: usize = 128;
    const HASH_ALGO: Algorithm = Algorithm::Sha512;
    const HMAC_ALGO: Algorithm = Algorithm::HmacSha512;

    const EMPTY_HASH: Self::HashType = [
        0xcf, 0x83, 0xe1, 0x35, 0x7e, 0xef, 0xb8, 0xbd, 0xf1, 0x54, 0x28, 0x50, 0xd6, 0x6d, 0x80,
        0x07, 0xd6, 0x20, 0xe4, 0x05, 0x0b, 0x57, 0x15, 0xdc, 0x83, 0xf4, 0xa9, 0x21, 0xd3, 0x6c,
        0xe9, 0xce, 0x47, 0xd0, 0xd1, 0x3c, 0x5d, 0x85, 0xf2, 0xb0, 0xff, 0x83, 0x18, 0xd2, 0x87,
        0x7e, 0xec, 0x2f, 0x63, 0xb9, 0x31, 0xbd, 0x47, 0x41, 0x7a, 0x81, 0xa5, 0x38, 0x32, 0x7a,
        0xf9, 0x27, 0xda, 0x3e,
    ];
}

bitflags! {
    #[derive(Debug, Copy, Clone)]
    pub struct SHAStatus: u32 {
        /// Data Ready (cleared by writing a 1 to bit `SWRST` or `START` in `CR`, or by reading `IODATARx`)
        const DATARDY = 1 << 0;
        /// Input Data Register Write Ready (1 means `IDATAR0` can be written)
        const WRDY    = 1 << 4;
        /// Unspecified Register Access Detection Status (cleared by writing a 1 to `CR.SWRST`)
        const URAD    = 1 << 8;
        /// Check Done Status (cleared by writing `CR.START` or `CR.SWRST` or by reading `IODATARx`)
        const CHECKF  = 1 << 16;
    }
}

#[derive(Debug, Copy, Clone)]
pub enum Algorithm {
    Sha1 = 0,
    Sha256 = 1,
    Sha384 = 2,
    Sha512 = 3,
    Sha224 = 4,
    HmacSha1 = 8,
    HmacSha256 = 9,
    HmacSha384 = 10,
    HmacSha512 = 11,
    HmacSha224 = 12,
}

impl Algorithm {
    pub(crate) fn hash_state_words(&self) -> usize {
        match self {
            Algorithm::Sha1 | Algorithm::HmacSha1 => 5,
            // SHA-224 uses a full 256-bit (8-word) internal state, same as SHA-256
            Algorithm::Sha224 | Algorithm::HmacSha224 => 8,
            Algorithm::Sha256 | Algorithm::HmacSha256 => 8,
            // SHA-384 uses a full 512-bit (16-word) internal state, same as SHA-512
            Algorithm::Sha384 | Algorithm::HmacSha384 => 16,
            Algorithm::Sha512 | Algorithm::HmacSha512 => 16,
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum Check {
    /// No check is performed.
    NoCheck = 0,
    /// Check is performed with expected hash stored in internal expected hash value
    /// registers.
    CheckExpectedHashValue = 1,
    /// Check is performed with expected hash provided after the message.
    CheckMessage = 2,
}

#[derive(Debug, Copy, Clone)]
pub enum StartMode {
    /// Manual mode
    Manual = 0,
    /// Auto mode
    Auto = 1,
    /// `IDATAR0` access only mode (mandatory when DMA is used)
    Idatar0 = 2,
}

#[derive(Debug, Copy, Clone)]
pub enum Buffering {
    /// Single buffer: IDATAR cannot be written to while processing
    Single = 0,
    /// Double buffer: IDATAR can be written to while processing
    Double = 1,
}

pub struct Sha {
    base_addr: u32,
}

impl Default for Sha {
    fn default() -> Self {
        Sha::new()
    }
}

impl Sha {
    pub const DMA_CONFIG: DmaPeripheralTransferConfig = DmaPeripheralTransferConfig {
        peripheral_id: DmaPeripheralId::Sha,
        direction: DmaTransferDirection::MemoryToPeripheral,
        data_width: DmaDataWidth::D32,
        chunk_size: DmaChunkSize::C16,
    };

    #[inline]
    pub fn new() -> Self {
        Self {
            base_addr: HW_SHA_BASE as u32,
        }
    }

    /// Creates SHA instance with a different base address. Used with virtual memory
    #[inline]
    pub fn with_alt_base_addr(base_addr: u32) -> Self {
        Self { base_addr }
    }

    #[inline]
    pub fn reset(&self) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        sha_csr.wfo(CR_SWRST, 1);
    }

    #[inline]
    pub fn status(&self) -> SHAStatus {
        let sha_csr = CSR::new(self.base_addr as *mut u32);
        SHAStatus::from_bits_truncate(sha_csr.r(ISR))
    }

    #[inline]
    fn set_mode_with_uihv(&self, algorithm: Algorithm, mode: StartMode, buffering: Buffering) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        let mr = sha_csr.ms(MR_SMOD, mode as u32)
            | sha_csr.ms(MR_ALGO, algorithm as u32)
            | sha_csr.ms(MR_DUALBUFF, buffering as u32)
            | sha_csr.ms(MR_UIHV, 1);
        sha_csr.wo(MR, mr);
    }

    fn clear_cr(&self) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        sha_csr.wo(CR, 0);
    }

    #[inline]
    pub fn set_mode(&self, algorithm: Algorithm, mode: StartMode, buffering: Buffering) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        let mr = sha_csr.ms(MR_SMOD, mode as u32)
            | sha_csr.ms(MR_ALGO, algorithm as u32)
            | sha_csr.ms(MR_DUALBUFF, buffering as u32);
        sha_csr.wo(MR, mr);
    }

    #[inline]
    pub fn set_message_size(&self, size: u32) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        sha_csr.wo(MSR, size);
    }

    /// When the hash processing starts from the beginning of a message (without
    /// preprocessed hash part), `BYTCNT` must be written with the same value as
    /// `MSGSIZE`. If a part of the message has been already hashed and the hash does
    /// not start from the beginning, `BYTCNT` must be configured with the number of bytes
    /// remaining to process before the padding section.
    ///
    /// When read, provides the size in bytes of the message remaining to be written
    /// before the automatic padding starts. `BYTCNT` is automatically updated each
    /// time a write occurs in `IDATARx` and `IODATARx`. When `BYTCNT` reaches 0, the
    /// `MSGSIZE` is converted into a bit count and appended at the end of the message
    /// after the padding, as described in the FIPS 180 specification.
    /// To disable automatic padding, the `MSGSIZE` and `BYTCNT` fields must be written to
    /// 0.
    #[inline]
    pub fn set_byte_count(&self, count: u32) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        sha_csr.wo(BCR, count);
    }

    #[inline]
    pub fn byte_count(&self) -> u32 {
        let sha_csr = CSR::new(self.base_addr as *mut u32);
        sha_csr.r(BCR)
    }

    #[inline]
    pub fn first(&self) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        sha_csr.wfo(CR_FIRST, 1);
    }

    #[inline]
    pub fn start(&self) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        sha_csr.wfo(CR_START, 1);
    }

    fn select_ir0(&self) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        sha_csr.wfo(CR_WUIHV, 1);
    }

    fn select_ir1(&self) {
        let mut sha_csr = CSR::new(self.base_addr as *mut u32);
        sha_csr.wfo(CR_WUIEHV, 1);
    }

    fn write_block(&self, block: &[u8]) {
        let idatar_base = (self.base_addr + IDATAR_OFFSET) as *mut u32;

        for (i, word) in block.chunks(4).enumerate() {
            let mut word_u32 = [0u8; 4];
            word_u32[..word.len()].copy_from_slice(word);
            let word_u32 = u32::from_le_bytes(word_u32);

            unsafe {
                idatar_base.add(i).write_volatile(word_u32);
            }
        }
    }

    fn read_result<H: HashTypeHelper>(&self) -> H {
        let iodatar_base = (self.base_addr + IODATAR_OFFSET) as *mut u32;

        let mut hash = H::default();
        for (i, word) in hash.as_mut_slice().chunks_exact_mut(4).enumerate() {
            let hash_word = unsafe { iodatar_base.add(i).read_volatile() };
            word.copy_from_slice(&hash_word.to_le_bytes());
        }
        hash
    }

    fn wait_data_ready(&self) {
        while !self.status().contains(SHAStatus::DATARDY) {}
    }

    #[inline]
    pub fn dma_in_address(&self) -> usize {
        (self.base_addr + IDATAR_OFFSET) as usize
    }

    #[inline]
    pub fn hash_dma<A: Algo, E>(
        &self,
        data_len: usize,
        dma_execute: impl Fn() -> Result<(), E>,
    ) -> Result<A::HashType, E> {
        if data_len == 0 {
            return Ok(A::EMPTY_HASH);
        }

        self.reset();
        self.set_mode(A::HASH_ALGO, StartMode::Idatar0, Buffering::Double);
        self.set_message_size(data_len as u32);
        self.set_byte_count(data_len as u32);
        self.first();
        dma_execute()?;
        self.wait_data_ready();
        Ok(self.read_result())
    }

    /// Initialize streaming SHA hash. Call this once at the start, then call
    /// [`Self::update_dma`] for each chunk, and finally [`Self::finalize`] to get the
    /// hash
    ///
    /// Pass `total_len = 0` to disable auto-padding (open-ended streaming); pass the
    /// actual byte count to enable HW padding and finalization.
    #[inline]
    pub fn init_streaming<A: Algo>(&self, total_len: usize) {
        self.reset();
        self.set_mode(A::HASH_ALGO, StartMode::Idatar0, Buffering::Double);
        self.set_message_size(total_len as u32);
        self.set_byte_count(total_len as u32);
        self.first();
    }

    /// Restore a previously saved SHA context and continue processing
    #[inline]
    pub fn restore_context(&self, ctx: &ShaHwContext) {
        self.reset();

        self.select_ir0();
        self.write_hash_state(ctx.algorithm, &ctx.hash_state);
        self.clear_cr();

        self.set_mode_with_uihv(ctx.algorithm, StartMode::Idatar0, Buffering::Double);
        self.set_message_size(ctx.total_len as u32);
        self.set_byte_count(ctx.bytes_remaining as u32);
        self.first();
    }

    /// Save the current SHA hardware context for later restoration.
    ///
    /// Can only be called once, as DATRDY clears on the first IODATAR read
    #[inline]
    pub fn save_context(&self, ctx: &mut ShaHwContext) {
        self.wait_data_ready();
        ctx.bytes_remaining = self.byte_count() as usize;
        ctx.hash_state = self.read_hash_state(ctx.algorithm);
    }

    fn write_hash_state(&self, algorithm: Algorithm, state: &[u8; 64]) {
        let idatar_base = (self.base_addr + IDATAR_OFFSET) as *mut u32;
        let words = algorithm.hash_state_words();
        for (i, word) in state.chunks_exact(4).take(words).enumerate() {
            let word_u32 = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            unsafe {
                idatar_base.add(i).write_volatile(word_u32);
            }
        }
    }

    fn read_hash_state(&self, algorithm: Algorithm) -> [u8; 64] {
        let iodatar_base = (self.base_addr + IODATAR_OFFSET) as *mut u32;
        let mut state = [0u8; 64];
        let words = algorithm.hash_state_words();
        for (i, word) in state.chunks_exact_mut(4).take(words).enumerate() {
            let hash_word = unsafe { iodatar_base.add(i).read_volatile() };
            word.copy_from_slice(&hash_word.to_le_bytes());
        }
        state
    }

    /// Finalize the streaming SHA hash and return the result
    ///
    /// Can only be called once, as DATRDY clears on the first IODATAR read
    #[inline]
    pub fn finalize<H: HashTypeHelper>(&self) -> H {
        self.wait_data_ready();
        self.read_result()
    }

    fn _hash_common<A: Algo>(&self, data: &[u8], auto_padding: bool) {
        self.reset();
        self.set_mode(A::HASH_ALGO, StartMode::Auto, Buffering::Single);
        self.first();
        if auto_padding {
            self.set_message_size(data.len() as u32);
            self.set_byte_count(data.len() as u32);
        }
        for block in data.chunks(A::BLOCK_SIZE) {
            self.write_block(block);
            self.wait_data_ready();
        }
    }

    fn _hash_ext<A: Algo>(&self, data: &[u8]) -> A::ExtHashType {
        self._hash_common::<A>(data, false);
        self.read_result()
    }

    #[inline]
    pub fn hash<A: Algo>(&self, data: &[u8]) -> A::HashType {
        if data.is_empty() {
            return A::EMPTY_HASH;
        }
        self._hash_common::<A>(data, true);
        self.read_result()
    }

    #[inline]
    pub fn hash_padded<A: Algo>(&self, data: &[u8]) -> A::HashType {
        if data.is_empty() {
            return A::EMPTY_HASH;
        }
        self._hash_common::<A>(data, false);
        self.read_result()
    }

    #[inline]
    pub fn hmac<A: Algo>(&self, key: &[u8], data: &[u8]) -> A::HashType {
        let mut block_size_key = A::BlockType::default();
        let block_size_key = block_size_key.as_mut_slice();
        if key.len() > A::BLOCK_SIZE {
            let hashed_key = self.hash::<A>(key);
            let hashed_key = hashed_key.as_slice();
            block_size_key[..hashed_key.len()].copy_from_slice(hashed_key);
        } else {
            block_size_key[..key.len()].copy_from_slice(key);
        }

        if data.is_empty() {
            block_size_key.iter_mut().for_each(|v| *v ^= HMAC_IPAD);
            let hashed_ipad = self.hash::<A>(block_size_key);
            let hashed_ipad = hashed_ipad.as_slice();
            block_size_key
                .iter_mut()
                .for_each(|v| *v ^= HMAC_IPAD ^ HMAC_OPAD);
            let mut s2 = [0u8; 128 + 64];
            s2[..block_size_key.len()].copy_from_slice(block_size_key);
            s2[block_size_key.len()..block_size_key.len() + hashed_ipad.len()]
                .copy_from_slice(hashed_ipad);
            return self.hash::<A>(&s2[..block_size_key.len() + hashed_ipad.len()]);
        }
        block_size_key.iter_mut().for_each(|v| *v ^= HMAC_IPAD);
        let ir0 = self._hash_ext::<A>(block_size_key);
        block_size_key
            .iter_mut()
            .for_each(|v| *v ^= HMAC_OPAD ^ HMAC_IPAD);
        let ir1 = self._hash_ext::<A>(block_size_key);

        self.reset();
        self.set_mode(A::HMAC_ALGO, StartMode::Auto, Buffering::Single);
        self.select_ir0();
        self.write_block(ir0.as_slice());
        self.select_ir1();
        self.write_block(ir1.as_slice());
        self.first();
        self.set_message_size(data.len() as u32);
        self.set_byte_count(data.len() as u32);
        for block in data.chunks(A::BLOCK_SIZE) {
            self.write_block(block);
            self.wait_data_ready();
        }
        self.read_result()
    }
}
