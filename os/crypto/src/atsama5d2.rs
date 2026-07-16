// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crypto::{error::CryptoError, messages::*, Direction, AES_BLOCK_SIZE};
use {
    atsama5d27::{
        aes::{Aes, Iv},
        pmc::PeripheralId,
        sha::{Algorithm, Sha, Sha224, Sha256, Sha384, Sha512, ShaHwContext},
    },
    dma::error::DmaError,
    securam_manager::SecuramManager,
    server::{
        xous::{flush_cache, syscall, CacheOperation, MemoryAddress, MemoryFlags, MemoryRange, PID},
        ScalarEventSubscriber, ServerContext,
    },
    std::collections::BTreeMap,
};

dma::use_api!();
power_manager::use_api!();

#[derive(server::Server)]
#[name = "os/crypto"]
pub(crate) struct CryptoServer {
    aes_contexts: BTreeMap<(PID, u8), AesContext>,
    next_context_id: u8,
    aes: Aes,
    sha: Sha,
    dma_aes_tx: DmaTransfer,
    dma_aes_rx: DmaTransfer,
    dma_disk_encrypt_tx: DmaTransfer,
    dma_disk_encrypt_rx: DmaTransfer,
    dma_sha: DmaTransfer,
    power_manager: PowerManagerApi,
    securam_manager: SecuramManager,
    securam_slot_occupied: [bool; securam_manager::NUM_SECURAM_AES_KEYS],
    sha_contexts: BTreeMap<(PID, usize), ShaContext>,
    last_sha_context_id: usize,
    disk_encrypt_subscriber: Option<ScalarEventSubscriber<DiskEncryptComplete>>,
    disk_encrypt_in_flight: bool,
}

#[derive(Clone)]
struct ShaContext {
    algo: ShaAlgo,
    hash_state: [u8; 64],
}

struct AesContext {
    key_slot: usize,
    finalized: bool,
    mode: AesContextMode,
}

#[derive(Debug, Clone)]
enum AesContextMode {
    Ecb,

    Cbc {
        iv: atsama5d27::aes::Iv,
    },
    Gcm {
        aadlen: usize,  // aad length processed so far
        datalen: usize, // data length processed so far
        iv: [u32; 3],
        ctr: u32,
        ghash: [u32; 4], // intermediate hash
    },
    Ctr {
        ctr: atsama5d27::aes::Iv,
    },
}

impl server::Server for CryptoServer {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        self.dma_disk_encrypt_rx
            .subscribe_transfer_complete(context)
            .expect("subscribe to os/dma TransferComplete");
    }
}

impl CryptoServer {
    pub fn new() -> Self {
        let aes_csr = syscall::map_memory(
            MemoryAddress::new(utralib::HW_AES_BASE),
            None,
            0x1000,
            MemoryFlags::W | MemoryFlags::DEV,
        )
        .unwrap();

        let sha_csr = syscall::map_memory(
            MemoryAddress::new(utralib::HW_SHA_BASE),
            None,
            0x1000,
            MemoryFlags::W | MemoryFlags::DEV,
        )
        .unwrap();

        let securam = syscall::map_memory(
            MemoryAddress::new(utralib::HW_SECURAM_MEM),
            None,
            0x1000,
            MemoryFlags::W | MemoryFlags::DEV,
        )
        .unwrap();

        let aes = Aes::with_alt_base_addr(aes_csr.as_ptr() as usize);
        let sha = Sha::with_alt_base_addr(sha_csr.as_ptr() as u32);
        let dma = Dma::default();

        let dma_aes_tx = dma.peripheral_transfer(aes.dma_tx_addr() as _, Aes::TX_DMA_CONFIG).unwrap();
        let dma_aes_rx = dma.peripheral_transfer(aes.dma_rx_addr() as _, Aes::RX_DMA_CONFIG).unwrap();
        let dma_disk_encrypt_tx =
            dma.peripheral_transfer(aes.dma_tx_addr() as _, Aes::TX_DMA_CONFIG).unwrap();
        let dma_disk_encrypt_rx =
            dma.peripheral_transfer(aes.dma_rx_addr() as _, Aes::RX_DMA_CONFIG).unwrap();
        let dma_sha = dma.peripheral_transfer(sha.dma_in_address() as _, Sha::DMA_CONFIG).unwrap();

        Self {
            aes_contexts: Default::default(),
            next_context_id: 1,
            power_manager: Default::default(),
            aes,
            sha,
            dma_aes_tx,
            dma_aes_rx,
            dma_disk_encrypt_tx,
            dma_disk_encrypt_rx,
            dma_sha,
            securam_manager: unsafe { SecuramManager::new(securam.as_mut_ptr()).unwrap() },
            securam_slot_occupied: Default::default(),
            sha_contexts: Default::default(),
            last_sha_context_id: 0,
            disk_encrypt_subscriber: None,
            disk_encrypt_in_flight: false,
        }
    }

    /// Block until any in-flight disk-encrypt DMA finishes. Other AES requests share the
    /// single AES peripheral, so they must wait here before reconfiguring the registers.
    fn ensure_aes_idle(&mut self) {
        if self.disk_encrypt_in_flight {
            self.dma_disk_encrypt_rx.wait().ok();
        }
    }

    pub fn aes_setup(&mut self, msg: AesSetup, sender: PID) -> Result<usize, CryptoError> {
        for _ in 0..255 {
            let id = self.next_context_id;
            self.next_context_id += 1;

            // Sorry Clippy, we use a &mut self method in the body of this if.
            #[allow(clippy::map_entry)]
            if !self.aes_contexts.contains_key(&(sender, id)) {
                let key_slot = self.allocate_securam_slot(&msg.key)?;
                let mode = match &msg.mode {
                    AesMode::Ecb => AesContextMode::Ecb,
                    AesMode::Cbc { iv } => AesContextMode::Cbc { iv: Iv::try_from_slice(iv).unwrap() },
                    AesMode::Ctr { iv } => AesContextMode::Ctr { ctr: Iv::try_from_slice(iv).unwrap() },
                    AesMode::Gcm { iv } => {
                        let mut iv32 = [0; 3];
                        for (i, chunk) in iv.chunks_exact(4).enumerate() {
                            iv32[i] = u32::from_le_bytes(chunk.try_into().unwrap());
                        }

                        AesContextMode::Gcm {
                            aadlen: 0,
                            datalen: 0,
                            iv: iv32,
                            ctr: 2, // J0 starts at 1, and is incremented before the first block
                            ghash: Default::default(),
                        }
                    }
                };

                self.aes_contexts.insert((sender, id), AesContext { key_slot, finalized: false, mode });
                return Ok(id as usize);
            }
        }
        Err(CryptoError::TooManyAesContexts)
    }

    fn allocate_securam_slot(&mut self, key: &[u8]) -> Result<usize, CryptoError> {
        let key_slot =
            self.securam_slot_occupied.iter().position(|s| !*s).ok_or(CryptoError::TooManySecuramKeys)?;
        if let Err(e) = self.securam_manager.set_aes_key(key_slot, key) {
            match e {
                securam_manager::Error::WrongKeySize => return Err(CryptoError::InvalidKeyLength),
                securam_manager::Error::MagicMismatch | securam_manager::Error::ChecksumMismatch => {
                    panic!("SECURAM is corrupted")
                }
            }
        }
        self.securam_slot_occupied[key_slot] = true;
        log::trace!("Allocated slot {key_slot}");
        Ok(key_slot)
    }

    fn deallocate_securam_slot(&mut self, key_slot: usize) {
        log::trace!("Deallocated slot {key_slot}");
        self.securam_manager.set_aes_key(key_slot, &[0; 32]).expect("SECURAM is corrupted");
        self.securam_slot_occupied[key_slot] = false;
    }

    pub fn aes_execute(&mut self, msg: AesExecute, sender: PID) -> Result<usize, CryptoError> {
        if (msg.offset + msg.len) > msg.buf.len() || msg.len == 0 {
            return Err(CryptoError::InvalidDataLength);
        }
        self.ensure_aes_idle();

        let context =
            self.aes_contexts.get_mut(&(sender, msg.transfer_id)).ok_or(CryptoError::InvalidParameter)?;

        if context.finalized {
            return Err(CryptoError::InvalidState);
        }

        if !matches!(context.mode, AesContextMode::Gcm { .. } | AesContextMode::Ctr { .. })
            && (msg.len % AES_BLOCK_SIZE) != 0
        {
            return Err(CryptoError::UnalignedDataLength);
        }

        let mut buf_part = msg.buf.subrange(msg.offset, msg.len).ok_or(CryptoError::InvalidParameter)?;

        let key = self.securam_manager.aes_key(context.key_slot).expect("SECURAM is corrupted");
        let mode = match &mut context.mode {
            AesContextMode::Ecb => atsama5d27::aes::AesMode::Ecb { key },
            AesContextMode::Cbc { iv } => {
                let original_iv = iv.clone();
                if msg.direction == Direction::Decrypt {
                    *iv = Iv::try_from_slice(
                        &buf_part.as_slice()[buf_part.len() - AES_BLOCK_SIZE..buf_part.len()],
                    )
                    .unwrap();
                }
                atsama5d27::aes::AesMode::Cbc { key, iv: original_iv }
            }
            AesContextMode::Gcm { iv, ctr, ghash, .. } => {
                atsama5d27::aes::AesMode::Gcm { key, iv: iv.clone(), ctr: *ctr, ghash: *ghash }
            }
            AesContextMode::Ctr { ctr } => {
                if ctr.is_ctr_rollover(msg.len / AES_BLOCK_SIZE) {
                    log::error!("Unsupported buffer size for CTR mode. Split the transfer up");
                    return Err(CryptoError::InvalidDataLength);
                }
                atsama5d27::aes::AesMode::Counter { key, ctr_value: ctr.clone() }
            }
        };

        self.power_manager.enable_peripheral(PeripheralId::Aes)?;
        match msg.direction {
            Direction::Encrypt => self.aes.init_encrypt(mode),
            Direction::Decrypt => self.aes.init_decrypt(mode),
        };
        self.aes.setup_for_dma(msg.len);

        let aligned_len = msg.len & !(AES_BLOCK_SIZE - 1);
        if aligned_len > 0 {
            let aligned_part = buf_part.subrange(0, aligned_len).ok_or(CryptoError::InvalidParameter)?;
            flush_cache(aligned_part, CacheOperation::CleanAndInvalidate).ok();
            unsafe {
                self.dma_aes_tx.execute(aligned_part).map_err(convert_dma_error)?;
                self.dma_aes_rx.execute(aligned_part).map_err(convert_dma_error)?;
            }
            self.dma_aes_rx.wait().map_err(convert_dma_error)?;
        }

        if aligned_len != msg.len {
            let mut padded_in = [0; AES_BLOCK_SIZE];
            let mut padded_out = [0x0; AES_BLOCK_SIZE];
            padded_in[..msg.len - aligned_len].copy_from_slice(&buf_part.as_slice()[aligned_len..]);
            self.aes.process(&padded_in, &mut padded_out);
            buf_part.as_slice_mut()[aligned_len..].copy_from_slice(&padded_out[..msg.len - aligned_len]);
            context.finalized = true;
        }

        match &mut context.mode {
            AesContextMode::Ecb => {}
            AesContextMode::Cbc { iv, .. } => {
                if msg.direction == Direction::Encrypt {
                    *iv = Iv::try_from_slice(
                        &buf_part.as_slice()[buf_part.len() - AES_BLOCK_SIZE..buf_part.len()],
                    )
                    .unwrap();
                }
            }
            AesContextMode::Gcm { ctr, ghash, datalen, .. } => {
                *ctr += (msg.len / AES_BLOCK_SIZE) as u32;
                *ghash = self.aes.get_ghash();
                *datalen += msg.len;
            }
            AesContextMode::Ctr { ctr } => {
                ctr.add(msg.len / AES_BLOCK_SIZE);
            }
        }

        self.power_manager.disable_peripheral(PeripheralId::Aes)?;

        Ok(msg.len)
    }

    pub fn aes_aad(&mut self, msg: AesAad, sender: PID) -> Result<usize, CryptoError> {
        self.ensure_aes_idle();
        let context =
            self.aes_contexts.get_mut(&(sender, msg.transfer_id)).ok_or(CryptoError::InvalidParameter)?;
        if context.finalized {
            return Err(CryptoError::InvalidState);
        }
        let key = self.securam_manager.aes_key(context.key_slot).expect("SECURAM is corrupted");
        let AesContextMode::Gcm { iv, ctr, ghash, aadlen, datalen } = &mut context.mode else {
            return Err(CryptoError::InvalidMode);
        };
        // GCM tag formula is GHASH(AAD || pad || C || pad || lengths); all AAD must precede
        // ciphertext or the tag won't be interoperable.
        if *datalen != 0 {
            return Err(CryptoError::InvalidState);
        }
        self.power_manager.enable_peripheral(PeripheralId::Aes)?;
        self.aes.init_encrypt(atsama5d27::aes::AesMode::Gcm {
            key,
            iv: iv.clone(),
            ctr: *ctr,
            ghash: *ghash,
        });
        self.aes.process_aad(&msg.aad);
        *ghash = self.aes.get_ghash();
        *aadlen += msg.aad.len();
        self.power_manager.disable_peripheral(PeripheralId::Aes)?;
        Ok(msg.aad.len())
    }

    pub fn aes_get_tag(&mut self, msg: AesGcmTag, sender: PID) -> Result<[u8; 16], CryptoError> {
        self.ensure_aes_idle();
        let context =
            self.aes_contexts.get_mut(&(sender, msg.transfer_id)).ok_or(CryptoError::InvalidParameter)?;
        let (iv, ctr, ghash, aadlen, datalen) = match &context.mode {
            AesContextMode::Gcm { iv, ctr, ghash, aadlen, datalen } => (*iv, *ctr, *ghash, *aadlen, *datalen),
            _ => return Err(CryptoError::InvalidMode),
        };
        // Seal the context: a second tag under the same nonce would leak the GHASH key (the GCM
        // "forbidden attack") and let an attacker forge tags.
        context.finalized = true;
        let key = self.securam_manager.aes_key(context.key_slot).expect("SECURAM is corrupted");
        self.power_manager.enable_peripheral(PeripheralId::Aes)?;
        self.aes.init_encrypt(atsama5d27::aes::AesMode::Gcm { key: key.clone(), iv, ctr, ghash });
        let mut postfix = [0; 16];
        postfix[4..8].copy_from_slice(&(aadlen * 8).to_be_bytes());
        postfix[12..16].copy_from_slice(&(datalen * 8).to_be_bytes());
        self.aes.process_aad(&postfix);
        let s = self.aes.get_ghash();
        let mut s_bytes = [0; 16];
        for (i, v) in s.iter().enumerate() {
            s_bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        self.aes
            .init_encrypt(atsama5d27::aes::AesMode::Counter { key, ctr_value: Iv::from_gcm_params(iv, 1) });
        let mut tag = [0; 16];
        self.aes.process(&s_bytes, &mut tag);
        Ok(tag)
    }

    pub fn set_disk_encrypt_subscriber(&mut self, sub: ScalarEventSubscriber<DiskEncryptComplete>) {
        self.disk_encrypt_subscriber = Some(sub);
    }

    // TODO (SFT-5088): If the keys are all zero, this should return an error
    pub fn disk_encrypt_start(&mut self, msg: DiskEncryptUnsafe, sender: PID) -> Result<(), CryptoError> {
        if (msg.len % AES_BLOCK_SIZE) != 0 || msg.len == 0 {
            return Err(CryptoError::InvalidDataLength);
        }
        self.power_manager.enable_peripheral(PeripheralId::Aes)?;
        self.ensure_aes_idle();

        match self.start_disk_encrypt_dmas(msg, sender) {
            Ok(()) => {
                self.disk_encrypt_in_flight = true;
                Ok(())
            }
            Err(e) => {
                self.power_manager.disable_peripheral(PeripheralId::Aes).ok();
                Err(e)
            }
        }
    }

    fn start_disk_encrypt_dmas(&mut self, msg: DiskEncryptUnsafe, sender: PID) -> Result<(), CryptoError> {
        let keys = self.securam_manager.disk_encryption_keys().expect("SECURAM is corrupted");
        if keys.0.is_zero() || keys.1.is_zero() {
            log::error!("start_disk_encrypt_dmas called before disk encryption keys were set");
            return Err(CryptoError::InvalidKeyLength);
        }
        let mode = atsama5d27::aes::AesMode::Xts { key1: keys.0, key2: keys.1, tweak: msg.tweak, j: msg.j };

        match msg.direction {
            Direction::Encrypt => self.aes.init_encrypt(mode),
            Direction::Decrypt => self.aes.init_decrypt(mode),
        };
        self.aes.setup_for_dma(msg.len);

        let src = unsafe { MemoryRange::new(msg.src, msg.len) }.map_err(|_| CryptoError::InvalidAddress)?;
        let dst = unsafe { MemoryRange::new(msg.dst, msg.len) }.map_err(|_| CryptoError::InvalidAddress)?;

        unsafe { self.dma_disk_encrypt_tx.execute_for_pid(src, sender) }
            .map_err(|_| CryptoError::DmaError)?;
        unsafe { self.dma_disk_encrypt_rx.execute_for_pid(dst, sender) }
            .map_err(|_| CryptoError::DmaError)?;
        Ok(())
    }

    pub fn on_dma_transfer_complete(&mut self, transfer_id: u32) {
        if !self.disk_encrypt_in_flight || transfer_id != self.dma_disk_encrypt_rx.id() as u32 {
            return;
        }
        self.disk_encrypt_in_flight = false;
        self.power_manager.disable_peripheral(PeripheralId::Aes).ok();
        if let Some(sub) = &self.disk_encrypt_subscriber {
            if let Err(e) = sub.send(&DiskEncryptComplete) {
                log::warn!("DiskEncryptComplete send failed: {e:?}");
            }
        }
    }

    pub fn aes_clear(&mut self, msg: AesClear, sender: PID) {
        if let Some(context) = self.aes_contexts.remove(&(sender, msg.0)) {
            self.deallocate_securam_slot(context.key_slot);
        }
    }

    pub fn hmac(&mut self, algo: ShaAlgo, key: &[u8], msg: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let needs_power = self.sha_contexts.is_empty();
        if needs_power {
            self.power_manager.enable_peripheral(PeripheralId::Sha)?;
        }
        let hash = match algo {
            ShaAlgo::Sha224 => self.sha.hmac::<Sha224>(key, msg).to_vec(),
            ShaAlgo::Sha256 => self.sha.hmac::<Sha256>(key, msg).to_vec(),
            ShaAlgo::Sha384 => self.sha.hmac::<Sha384>(key, msg).to_vec(),
            ShaAlgo::Sha512 => self.sha.hmac::<Sha512>(key, msg).to_vec(),
        };
        if needs_power {
            self.power_manager.disable_peripheral(PeripheralId::Sha)?;
        }
        Ok(hash)
    }

    pub fn sha_set_context(&mut self, sender: PID, msg: ShaSetContext) -> Result<usize, CryptoError> {
        if let Some(existing_id) = msg.context_id {
            if let Some(ctx) = self.sha_contexts.get_mut(&(sender, existing_id)) {
                ctx.hash_state = msg.hash_state;
                return Ok(existing_id);
            }
            return Err(CryptoError::InvalidParameter);
        }

        if self.sha_contexts.is_empty() {
            self.power_manager.enable_peripheral(PeripheralId::Sha)?;
        }
        self.last_sha_context_id += 1;
        let id = self.last_sha_context_id;
        self.sha_contexts.insert((sender, id), ShaContext { algo: msg.algo, hash_state: msg.hash_state });
        Ok(id)
    }

    pub fn sha_update(
        &mut self,
        sender: PID,
        context_id: usize,
        buf: MemoryRange,
        length: usize,
    ) -> Result<usize, CryptoError> {
        let context =
            self.sha_contexts.get(&(sender, context_id)).ok_or(CryptoError::InvalidParameter)?.clone();

        let mut hw_ctx = ShaHwContext::new(convert_sha_algo(context.algo), 0);
        hw_ctx.hash_state = context.hash_state;

        self.sha.restore_context(&hw_ctx);

        let dma_range = buf.subrange(0, length).ok_or(CryptoError::InvalidParameter)?;
        flush_cache(dma_range, CacheOperation::Clean)?;

        unsafe { self.dma_sha.execute(dma_range).map_err(convert_dma_error)? };
        self.dma_sha.wait().map_err(convert_dma_error)?;

        self.sha.save_context(&mut hw_ctx);

        if let Some(ctx) = self.sha_contexts.get_mut(&(sender, context_id)) {
            ctx.hash_state = hw_ctx.hash_state;
        }

        Ok(length)
    }

    pub fn sha_get_context(
        &mut self,
        sender: PID,
        context_id: usize,
    ) -> Result<ShaContextSnapshot, CryptoError> {
        let context = self.sha_contexts.get(&(sender, context_id)).ok_or(CryptoError::InvalidParameter)?;
        Ok(ShaContextSnapshot { algo: context.algo, hash_state: context.hash_state })
    }

    pub fn sha_drop(&mut self, sender: PID, context_id: usize) {
        if self.sha_contexts.remove(&(sender, context_id)).is_some() && self.sha_contexts.is_empty() {
            self.power_manager.disable_peripheral(PeripheralId::Sha).ok();
        }
    }
}

fn convert_sha_algo(value: ShaAlgo) -> Algorithm {
    match value {
        ShaAlgo::Sha224 => Algorithm::Sha224,
        ShaAlgo::Sha256 => Algorithm::Sha256,
        ShaAlgo::Sha384 => Algorithm::Sha384,
        ShaAlgo::Sha512 => Algorithm::Sha512,
    }
}

fn convert_dma_error(value: DmaError) -> CryptoError {
    match value {
        DmaError::XousError(e) => CryptoError::XousError(e),
        DmaError::InvalidParameter => CryptoError::InvalidParameter,
        DmaError::InvalidAddress => CryptoError::InvalidAddress,
        DmaError::InvalidAlignment => CryptoError::InvalidDataLength,
        DmaError::BufferNotContiguous => CryptoError::BufferNotContiguous,
        _ => CryptoError::DmaError,
    }
}
