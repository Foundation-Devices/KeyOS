// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;

use aes::{
    cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit},
    Aes128, Aes192, Aes256,
};
use crypto::{messages::*, Direction, AES_BLOCK_SIZE};
use ghash::{universal_hash::UniversalHash, GHash};
use hmac::Mac;
use server::xous::PID;
use server::MessageId as _;
use sha2::{Sha224, Sha256, Sha384, Sha512};
use zeroize::{Zeroize, Zeroizing};

use crate::{CryptoError, ShaAlgo};

/// Kernel-delivered when a hosted client disconnects or exits; the scalar payload is unused
/// because the message's sender is the departed pid.
#[derive(Debug, server::Message)]
pub(crate) struct ClientDisconnected(server::xous::CID);

#[derive(server::Server)]
#[name = "os/crypto"]
pub struct CryptoServer {
    aes_contexts: BTreeMap<(PID, u8), AesContext>,
    next_context_id: u8,
}

struct AesContext {
    key: Vec<u8>,
    got_unaligned_execute: bool,
    finalized: bool,
    mode: AesContextMode,
}

enum AesContextMode {
    Ecb,
    Cbc {
        iv: [u8; AES_BLOCK_SIZE],
    },
    Gcm {
        aadlen: usize,
        aad_partial_len: usize,
        aad_partial: [u8; AES_BLOCK_SIZE],
        datalen: usize,
        iv: [u8; 12],
        ctr: u32,
        ghash: GHash,
    },
    Ctr {
        ctr: [u8; AES_BLOCK_SIZE],
    },
}

impl AesContextMode {
    fn zeroize_sensitive(&mut self) {
        if let AesContextMode::Gcm { aad_partial, .. } = self {
            aad_partial.zeroize();
        }
    }
}

impl server::Server for CryptoServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        server::xous::register_server_event_handler(
            server::xous::ServerEvent::Disconnected,
            context.sid(),
            crate::ClientDisconnected::ID,
        )
        .expect("register hosted os/crypto disconnect handler");
    }
}

impl Default for CryptoServer {
    fn default() -> Self { Self::new() }
}

impl CryptoServer {
    pub fn new() -> Self { Self { aes_contexts: BTreeMap::new(), next_context_id: 1 } }

    pub fn aes_setup(&mut self, mut msg: AesSetup, sender: PID) -> Result<usize, CryptoError> {
        validate_aes_key(&msg.key)?;
        let mut key = Zeroizing::new(std::mem::take(&mut msg.key));
        let mode = std::mem::replace(&mut msg.mode, AesMode::Ecb);

        for _ in 0..255 {
            let id = self.next_context_id;
            self.next_context_id = self.next_context_id.wrapping_add(1);

            #[allow(clippy::map_entry)]
            if !self.aes_contexts.contains_key(&(sender, id)) {
                let mode = match mode {
                    AesMode::Ecb => AesContextMode::Ecb,
                    AesMode::Cbc { iv } => AesContextMode::Cbc { iv },
                    AesMode::Ctr { iv } => AesContextMode::Ctr { ctr: iv },
                    AesMode::Gcm { iv } => {
                        let mut hash_subkey: [u8; AES_BLOCK_SIZE] = [0; AES_BLOCK_SIZE];
                        encrypt_block(&key, &mut hash_subkey)?;
                        let ghash = GHash::new_with_init_block(&hash_subkey.into(), 0);
                        hash_subkey.zeroize();
                        AesContextMode::Gcm {
                            aadlen: 0,
                            aad_partial_len: 0,
                            aad_partial: [0; AES_BLOCK_SIZE],
                            datalen: 0,
                            iv,
                            ctr: 2,
                            ghash,
                        }
                    }
                };

                self.aes_contexts.insert(
                    (sender, id),
                    AesContext {
                        key: std::mem::take(&mut *key),
                        got_unaligned_execute: false,
                        finalized: false,
                        mode,
                    },
                );
                return Ok(id as usize);
            }
        }

        Err(CryptoError::TooManyAesContexts)
    }

    pub fn aes_execute(&mut self, msg: AesExecute, sender: PID) -> Result<usize, CryptoError> {
        let Some(end) = msg.offset.checked_add(msg.len) else {
            return Err(CryptoError::InvalidDataLength);
        };
        if end > msg.buf.len() || msg.len == 0 {
            return Err(CryptoError::InvalidDataLength);
        }

        let context =
            self.aes_contexts.get_mut(&(sender, msg.transfer_id)).ok_or(CryptoError::InvalidParameter)?;

        if context.got_unaligned_execute || context.finalized {
            return Err(CryptoError::InvalidState);
        }

        if !matches!(context.mode, AesContextMode::Gcm { .. } | AesContextMode::Ctr { .. })
            && (msg.len % AES_BLOCK_SIZE) != 0
        {
            return Err(CryptoError::UnalignedDataLength);
        }

        let mut buf_part = msg.buf.subrange(msg.offset, msg.len).ok_or(CryptoError::InvalidParameter)?;

        match &mut context.mode {
            AesContextMode::Ecb => process_ecb(&context.key, buf_part.as_slice_mut(), msg.direction)?,
            AesContextMode::Cbc { iv } => {
                process_cbc(&context.key, iv, buf_part.as_slice_mut(), msg.direction)?
            }
            AesContextMode::Ctr { ctr } => {
                if ctr_rollover(*ctr, msg.len / AES_BLOCK_SIZE) {
                    return Err(CryptoError::InvalidDataLength);
                }
                process_ctr(&context.key, *ctr, buf_part.as_slice_mut())?;
                add_blocks(ctr, msg.len / AES_BLOCK_SIZE);
            }
            AesContextMode::Gcm { iv, ctr, ghash, aad_partial_len, aad_partial, datalen, .. } => {
                if *aad_partial_len != 0 {
                    ghash_block(ghash, to_padded_block(&aad_partial[..*aad_partial_len]));
                    *aad_partial_len = 0;
                    aad_partial.fill(0);
                }
                process_gcm(&context.key, *iv, *ctr, ghash, buf_part.as_slice_mut(), msg.direction)?;
                *ctr += (msg.len / AES_BLOCK_SIZE) as u32;
                *datalen += msg.len;
            }
        }

        if (msg.len % AES_BLOCK_SIZE) != 0 {
            context.got_unaligned_execute = true;
        }

        Ok(msg.len)
    }

    pub fn aes_aad(&mut self, msg: AesAad, sender: PID) -> Result<usize, CryptoError> {
        let context =
            self.aes_contexts.get_mut(&(sender, msg.transfer_id)).ok_or(CryptoError::InvalidParameter)?;
        let AesContextMode::Gcm { aadlen, aad_partial_len, aad_partial, datalen, ghash, .. } =
            &mut context.mode
        else {
            return Err(CryptoError::InvalidMode);
        };

        if context.finalized {
            return Err(CryptoError::InvalidState);
        }

        if *datalen != 0 {
            return Err(CryptoError::InvalidState);
        }

        let mut remaining = msg.aad.as_slice();
        if *aad_partial_len != 0 {
            let fill_len = (AES_BLOCK_SIZE - *aad_partial_len).min(remaining.len());
            aad_partial[*aad_partial_len..*aad_partial_len + fill_len]
                .copy_from_slice(&remaining[..fill_len]);
            *aad_partial_len += fill_len;
            remaining = &remaining[fill_len..];
            if *aad_partial_len == AES_BLOCK_SIZE {
                ghash_block(ghash, *aad_partial);
                *aad_partial_len = 0;
                aad_partial.fill(0);
            }
        }

        let (full_blocks, tail) = remaining.as_chunks::<AES_BLOCK_SIZE>();
        for block in full_blocks {
            ghash_block(ghash, *block);
        }
        if !tail.is_empty() {
            aad_partial[..tail.len()].copy_from_slice(tail);
            *aad_partial_len = tail.len();
        }
        *aadlen += msg.aad.len();
        Ok(msg.aad.len())
    }

    pub fn aes_get_tag(&mut self, msg: AesGcmTag, sender: PID) -> Result<[u8; 16], CryptoError> {
        let context =
            self.aes_contexts.get_mut(&(sender, msg.transfer_id)).ok_or(CryptoError::InvalidParameter)?;
        if context.finalized {
            return Err(CryptoError::InvalidState);
        }
        let AesContextMode::Gcm { aadlen, aad_partial_len, aad_partial, datalen, iv, ghash, .. } =
            &context.mode
        else {
            return Err(CryptoError::InvalidMode);
        };

        let mut ghash = ghash.clone();
        if *aad_partial_len != 0 {
            ghash_block(&mut ghash, to_padded_block(&aad_partial[..*aad_partial_len]));
        }
        let mut lengths = [0; AES_BLOCK_SIZE];
        lengths[4..8].copy_from_slice(&((*aadlen * 8) as u32).to_be_bytes());
        lengths[12..16].copy_from_slice(&((*datalen * 8) as u32).to_be_bytes());
        ghash_block(&mut ghash, lengths);

        let mut tag = gcm_pre_counter_block(*iv);
        encrypt_block(&context.key, &mut tag)?;
        xor_in_place(&mut tag, &ghash.finalize());
        context.finalized = true;
        Ok(tag)
    }

    pub fn aes_clear(&mut self, id: AesClear, sender: PID) {
        if let Some(mut context) = self.aes_contexts.remove(&(sender, id.0)) {
            context.mode.zeroize_sensitive();
            context.key.zeroize();
        }
    }

    pub fn clear_client_contexts(&mut self, pid: PID) {
        let aes_ids: Vec<u8> =
            self.aes_contexts.keys().filter(|(owner, _)| *owner == pid).map(|(_, id)| *id).collect();
        for id in aes_ids {
            if let Some(mut context) = self.aes_contexts.remove(&(pid, id)) {
                context.mode.zeroize_sensitive();
                context.key.zeroize();
            }
        }
    }

    pub fn hmac(&self, algo: ShaAlgo, key: &[u8], msg: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Ok(match algo {
            ShaAlgo::Sha224 => {
                type HmacSha224 = hmac::Hmac<Sha224>;
                let mut mac =
                    <HmacSha224 as Mac>::new_from_slice(key).map_err(|_| CryptoError::InvalidParameter)?;
                mac.update(msg);
                mac.finalize().into_bytes().to_vec()
            }
            ShaAlgo::Sha256 => {
                type HmacSha256 = hmac::Hmac<Sha256>;
                let mut mac =
                    <HmacSha256 as Mac>::new_from_slice(key).map_err(|_| CryptoError::InvalidParameter)?;
                mac.update(msg);
                mac.finalize().into_bytes().to_vec()
            }
            ShaAlgo::Sha384 => {
                type HmacSha384 = hmac::Hmac<Sha384>;
                let mut mac =
                    <HmacSha384 as Mac>::new_from_slice(key).map_err(|_| CryptoError::InvalidParameter)?;
                mac.update(msg);
                mac.finalize().into_bytes().to_vec()
            }
            ShaAlgo::Sha512 => {
                type HmacSha512 = hmac::Hmac<Sha512>;
                let mut mac =
                    <HmacSha512 as Mac>::new_from_slice(key).map_err(|_| CryptoError::InvalidParameter)?;
                mac.update(msg);
                mac.finalize().into_bytes().to_vec()
            }
        })
    }
}

fn validate_aes_key(key: &[u8]) -> Result<(), CryptoError> {
    match key.len() {
        16 | 24 | 32 => Ok(()),
        _ => Err(CryptoError::InvalidKeyLength),
    }
}

fn encrypt_block(key: &[u8], block: &mut [u8; AES_BLOCK_SIZE]) -> Result<(), CryptoError> {
    apply_block(key, block, true)
}

fn decrypt_block(key: &[u8], block: &mut [u8; AES_BLOCK_SIZE]) -> Result<(), CryptoError> {
    apply_block(key, block, false)
}

fn apply_block_with<C>(cipher: &C, out: &mut [u8; AES_BLOCK_SIZE], encrypt: bool)
where
    C: BlockEncrypt + BlockDecrypt,
{
    let mut block = GenericArray::clone_from_slice(out);
    if encrypt {
        cipher.encrypt_block(&mut block);
    } else {
        cipher.decrypt_block(&mut block);
    }
    out.copy_from_slice(&block);
}

fn apply_block(key: &[u8], out: &mut [u8; AES_BLOCK_SIZE], encrypt: bool) -> Result<(), CryptoError> {
    match key.len() {
        16 => {
            let cipher = Aes128::new(GenericArray::from_slice(key));
            apply_block_with(&cipher, out, encrypt);
            Ok(())
        }
        24 => {
            let cipher = Aes192::new(GenericArray::from_slice(key));
            apply_block_with(&cipher, out, encrypt);
            Ok(())
        }
        32 => {
            let cipher = Aes256::new(GenericArray::from_slice(key));
            apply_block_with(&cipher, out, encrypt);
            Ok(())
        }
        _ => Err(CryptoError::InvalidKeyLength),
    }
}

fn process_ecb(key: &[u8], buf: &mut [u8], direction: Direction) -> Result<(), CryptoError> {
    for chunk in buf.chunks_exact_mut(AES_BLOCK_SIZE) {
        let mut block = [0; AES_BLOCK_SIZE];
        block.copy_from_slice(chunk);
        match direction {
            Direction::Encrypt => encrypt_block(key, &mut block)?,
            Direction::Decrypt => decrypt_block(key, &mut block)?,
        }
        chunk.copy_from_slice(&block);
    }
    Ok(())
}

fn process_cbc(
    key: &[u8],
    iv: &mut [u8; AES_BLOCK_SIZE],
    buf: &mut [u8],
    direction: Direction,
) -> Result<(), CryptoError> {
    match direction {
        Direction::Encrypt => {
            for chunk in buf.chunks_exact_mut(AES_BLOCK_SIZE) {
                xor_in_place(chunk, iv);
                let mut block = [0; AES_BLOCK_SIZE];
                block.copy_from_slice(chunk);
                encrypt_block(key, &mut block)?;
                chunk.copy_from_slice(&block);
                *iv = block;
            }
        }
        Direction::Decrypt => {
            for chunk in buf.chunks_exact_mut(AES_BLOCK_SIZE) {
                let mut ciphertext = [0; AES_BLOCK_SIZE];
                ciphertext.copy_from_slice(chunk);
                let mut block = ciphertext;
                decrypt_block(key, &mut block)?;
                xor_in_place(&mut block, iv);
                chunk.copy_from_slice(&block);
                *iv = ciphertext;
            }
        }
    }
    Ok(())
}

fn process_ctr(key: &[u8], initial_ctr: [u8; AES_BLOCK_SIZE], buf: &mut [u8]) -> Result<(), CryptoError> {
    let mut ctr = initial_ctr;
    for chunk in buf.chunks_mut(AES_BLOCK_SIZE) {
        let mut stream = ctr;
        encrypt_block(key, &mut stream)?;
        for (dst, src) in chunk.iter_mut().zip(stream.iter()) {
            *dst ^= *src;
        }
        if chunk.len() == AES_BLOCK_SIZE {
            add_blocks(&mut ctr, 1);
        }
    }
    Ok(())
}

fn process_gcm(
    key: &[u8],
    iv: [u8; 12],
    initial_ctr: u32,
    ghash: &mut GHash,
    buf: &mut [u8],
    direction: Direction,
) -> Result<(), CryptoError> {
    let mut ctr = initial_ctr;
    for chunk in buf.chunks_mut(AES_BLOCK_SIZE) {
        let input_block = to_padded_block(chunk);
        let mut stream = counter_block(iv, ctr);
        encrypt_block(key, &mut stream)?;

        for (dst, src) in chunk.iter_mut().zip(stream.iter()) {
            *dst ^= *src;
        }

        let ghash_block_data = match direction {
            Direction::Encrypt => to_padded_block(chunk),
            Direction::Decrypt => input_block,
        };
        ghash_block(ghash, ghash_block_data);

        if chunk.len() == AES_BLOCK_SIZE {
            ctr = ctr.wrapping_add(1);
        }
    }
    Ok(())
}

fn xor_in_place(dst: &mut [u8], src: &[u8]) {
    for (dst, src) in dst.iter_mut().zip(src.iter()) {
        *dst ^= *src;
    }
}

fn to_padded_block(data: &[u8]) -> [u8; AES_BLOCK_SIZE] {
    let mut block = [0; AES_BLOCK_SIZE];
    block[..data.len()].copy_from_slice(data);
    block
}

fn counter_block(iv: [u8; 12], ctr: u32) -> [u8; AES_BLOCK_SIZE] {
    let mut block = [0; AES_BLOCK_SIZE];
    block[..12].copy_from_slice(&iv);
    block[12..].copy_from_slice(&ctr.to_be_bytes());
    block
}

fn gcm_pre_counter_block(iv: [u8; 12]) -> [u8; AES_BLOCK_SIZE] { counter_block(iv, 1) }

fn add_blocks(ctr: &mut [u8; AES_BLOCK_SIZE], aes_blocks: usize) {
    let mut to_add = aes_blocks as u128;
    for part in ctr.chunks_exact_mut(4).rev() {
        let word = u32::from_be_bytes(part.try_into().unwrap());
        let (new, overflow) = word.overflowing_add(to_add as u32);
        part.copy_from_slice(&new.to_be_bytes());
        if overflow {
            to_add = 1;
        } else {
            break;
        }
    }
}

fn ctr_rollover(ctr: [u8; AES_BLOCK_SIZE], blocks: usize) -> bool {
    const CTR_COUNTER_WORD_START: usize = 12;
    const CTR_COUNTER_WORD_END: usize = 16;
    const HARDWARE_CTR_COUNTER_MASK: u32 = 0xFFFF;
    const HARDWARE_CTR_COUNTER_LIMIT: u32 = 0x10000;

    let counter_word =
        u32::from_be_bytes(ctr[CTR_COUNTER_WORD_START..CTR_COUNTER_WORD_END].try_into().unwrap());
    let hardware_counter = counter_word & HARDWARE_CTR_COUNTER_MASK;
    (hardware_counter + blocks as u32) >= HARDWARE_CTR_COUNTER_LIMIT
}

fn ghash_block(ghash: &mut GHash, block: [u8; AES_BLOCK_SIZE]) { ghash.update(&[block.into()]); }

#[cfg(test)]
mod tests {
    use server::xous::MemoryRange;

    use super::*;

    fn pid() -> PID { PID::new(1).unwrap() }

    fn other_pid() -> PID { PID::new(2).unwrap() }

    fn mem_range(buf: &mut [u8]) -> MemoryRange {
        // SAFETY: The tests keep `buf` alive and exclusively borrowed for the entire
        // `MemoryRange` use, so the pointer and length describe a valid mutable slice.
        unsafe { MemoryRange::new(buf.as_mut_ptr() as usize, buf.len()).unwrap() }
    }

    fn hex(input: &str) -> Vec<u8> {
        assert_eq!(input.len() % 2, 0);
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = core::str::from_utf8(pair).unwrap();
                u8::from_str_radix(pair, 16).unwrap()
            })
            .collect()
    }

    // NIST SP 800-38A, F.1.1 ECB-AES128.Encrypt, PDF page 24:
    // https://doi.org/10.6028/NIST.SP.800-38A
    #[test]
    fn ecb_matches_nist_vector() {
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let plaintext = hex("6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef\
             f69f2445df4f9b17ad2b417be66c3710");
        let expected_ciphertext = hex("3ad77bb40d7a3660a89ecaf32466ef97\
             f5d3d58503b9699de785895a96fdbaaf\
             43b1cd7f598ece23881b00e3ed030688\
             7b0c785e27e8ad3f8223207104725dd4");

        let mut ciphertext = plaintext.clone();
        process_ecb(&key, &mut ciphertext, Direction::Encrypt).unwrap();
        assert_eq!(ciphertext, expected_ciphertext);

        process_ecb(&key, &mut ciphertext, Direction::Decrypt).unwrap();
        assert_eq!(ciphertext, plaintext);
    }

    // NIST SP 800-38A, F.2.1 CBC-AES128.Encrypt, PDF page 27:
    // https://doi.org/10.6028/NIST.SP.800-38A
    #[test]
    fn cbc_matches_nist_vector() {
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv: [u8; AES_BLOCK_SIZE] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let plaintext = hex("6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef\
             f69f2445df4f9b17ad2b417be66c3710");
        let expected_ciphertext = hex("7649abac8119b246cee98e9b12e9197d\
             5086cb9b507219ee95db113a917678b2\
             73bed6b8e3c1743b7116e69e22229516\
             3ff1caa1681fac09120eca307586e1a7");

        let mut ciphertext = plaintext.clone();
        let mut encrypt_iv = iv;
        process_cbc(&key, &mut encrypt_iv, &mut ciphertext, Direction::Encrypt).unwrap();
        assert_eq!(ciphertext, expected_ciphertext);

        let mut decrypt_iv = iv;
        process_cbc(&key, &mut decrypt_iv, &mut ciphertext, Direction::Decrypt).unwrap();
        assert_eq!(ciphertext, plaintext);
    }

    // NIST SP 800-38A, F.5.1 CTR-AES128.Encrypt, PDF page 55:
    // https://doi.org/10.6028/NIST.SP.800-38A
    #[test]
    fn ctr_matches_nist_vector() {
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let initial_ctr: [u8; AES_BLOCK_SIZE] = hex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff").try_into().unwrap();
        let plaintext = hex("6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef\
             f69f2445df4f9b17ad2b417be66c3710");
        let expected_ciphertext = hex("874d6191b620e3261bef6864990db6ce\
             9806f66b7970fdff8617187bb9fffdff\
             5ae4df3edbd5d35e5b4f09020db03eab\
             1e031dda2fbe03d1792170a0f3009cee");

        let mut ciphertext = plaintext.clone();
        process_ctr(&key, initial_ctr, &mut ciphertext).unwrap();
        assert_eq!(ciphertext, expected_ciphertext);

        process_ctr(&key, initial_ctr, &mut ciphertext).unwrap();
        assert_eq!(ciphertext, plaintext);
    }

    #[test]
    fn ctr_allows_partial_final_block_roundtrip() {
        let key = [0x11; 16];
        let ctr = [0x22; 16];
        let plaintext = b"hosted aes ctr partial".to_vec();

        let mut ciphertext = plaintext.clone();
        process_ctr(&key, ctr, &mut ciphertext).unwrap();
        assert_ne!(ciphertext, plaintext);

        process_ctr(&key, ctr, &mut ciphertext).unwrap();
        assert_eq!(ciphertext, plaintext);
    }

    #[test]
    fn ctr_state_matches_single_shot_across_multiple_executes() {
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv: [u8; AES_BLOCK_SIZE] = hex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff").try_into().unwrap();
        let plaintext = hex("6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef\
             f69f2445df4f9b17ad2b417be66c3710");

        let mut single_shot = plaintext.clone();
        process_ctr(&key, iv, &mut single_shot).unwrap();

        let sender = pid();
        let mut server = CryptoServer::new();
        let transfer_id =
            server.aes_setup(AesSetup { key, mode: AesMode::Ctr { iv } }, sender).unwrap() as u8;
        let mut split = plaintext;
        server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut split),
                    transfer_id,
                    direction: Direction::Encrypt,
                    offset: 0,
                    len: AES_BLOCK_SIZE,
                },
                sender,
            )
            .unwrap();
        server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut split),
                    transfer_id,
                    direction: Direction::Encrypt,
                    offset: AES_BLOCK_SIZE,
                    len: split.len() - AES_BLOCK_SIZE,
                },
                sender,
            )
            .unwrap();

        assert_eq!(split, single_shot);
    }

    #[test]
    fn clear_client_contexts_removes_only_disconnected_pid_contexts() {
        let sender = pid();
        let other = other_pid();
        let mut server = CryptoServer::new();

        let first_id =
            server.aes_setup(AesSetup { key: vec![0x11; 16], mode: AesMode::Ecb }, sender).unwrap() as u8;
        let second_id =
            server.aes_setup(AesSetup { key: vec![0x22; 16], mode: AesMode::Ecb }, other).unwrap() as u8;

        assert!(server.aes_contexts.contains_key(&(sender, first_id)));
        assert!(server.aes_contexts.contains_key(&(other, second_id)));

        server.clear_client_contexts(sender);

        assert!(!server.aes_contexts.contains_key(&(sender, first_id)));
        assert!(server.aes_contexts.contains_key(&(other, second_id)));
    }

    // McGrew and Viega, "The Galois/Counter Mode of Operation (GCM)",
    // Appendix B "AES Test Vectors", Test Case 2, PDF page 27:
    // https://luca-giuzzi.unibs.it/corsi/Support/papers-cryptography/gcm-spec.pdf
    #[test]
    fn gcm_matches_known_vector() {
        let key = hex("00000000000000000000000000000000");
        let iv: [u8; 12] = hex("000000000000000000000000").try_into().unwrap();
        let plaintext = hex("00000000000000000000000000000000");
        let expected_ciphertext = hex("0388dace60b6a392f328c2b971b2fe78");
        let expected_tag: [u8; 16] = hex("ab6e47d42cec13bdf53a67b21257bddf").try_into().unwrap();

        let mut hash_subkey = [0; AES_BLOCK_SIZE];
        encrypt_block(&key, &mut hash_subkey).unwrap();

        let mut ciphertext = plaintext.clone();
        let mut ghash = GHash::new_with_init_block(&hash_subkey.into(), 0);
        process_gcm(&key, iv, 2, &mut ghash, &mut ciphertext, Direction::Encrypt).unwrap();
        assert_eq!(ciphertext, expected_ciphertext);

        let mut lengths = [0; AES_BLOCK_SIZE];
        lengths[12..16].copy_from_slice(&((plaintext.len() * 8) as u32).to_be_bytes());
        ghash_block(&mut ghash, lengths);

        let mut tag = gcm_pre_counter_block(iv);
        encrypt_block(&key, &mut tag).unwrap();
        xor_in_place(&mut tag, &ghash.finalize());
        assert_eq!(tag, expected_tag);

        let mut decrypted = ciphertext;
        let mut decrypt_ghash = GHash::new_with_init_block(&hash_subkey.into(), 0);
        process_gcm(&key, iv, 2, &mut decrypt_ghash, &mut decrypted, Direction::Decrypt).unwrap();
        assert_eq!(decrypted, plaintext);
        ghash_block(&mut decrypt_ghash, lengths);
        let mut decrypt_tag = gcm_pre_counter_block(iv);
        encrypt_block(&key, &mut decrypt_tag).unwrap();
        xor_in_place(&mut decrypt_tag, &decrypt_ghash.finalize());
        assert_eq!(decrypt_tag, expected_tag);
    }

    // McGrew and Viega, "The Galois/Counter Mode of Operation (GCM)", Test Case 4.
    // Vector source (test_cases[3]): https://boringssl.googlesource.com/boringssl/+/3ffd70ec3692f577a94729/crypto/modes/gcm_test.c#92
    #[test]
    fn gcm_server_matches_multi_block_nist_vector() {
        let key = hex("feffe9928665731c6d6a8f9467308308");
        let iv: [u8; 12] = hex("cafebabefacedbaddecaf888").try_into().unwrap();
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let mut plaintext = hex("d9313225f88406e5a55909c5aff5269a\
             86a7a9531534f7da2e4c303d8a318a72\
             1c3c0c95956809532fcf0e2449a6b525\
             b16aedf5aa0de657ba637b39");
        let expected_ciphertext = hex("42831ec2217774244b7221b784d0d49c\
             e3aa212f2c02a4e035c17e2329aca12e\
             21d514b25466931c7d8f6a5aac84aa05\
             1ba30b396a0aac973d58e091");
        let expected_tag: [u8; 16] = hex("5bc94fbc3221a5db94fae95ae7121a47").try_into().unwrap();

        let sender = pid();
        let mut server = CryptoServer::new();
        let transfer_id =
            server.aes_setup(AesSetup { key, mode: AesMode::Gcm { iv } }, sender).unwrap() as u8;

        server.aes_aad(AesAad { transfer_id, aad: aad[..7].to_vec() }, sender).unwrap();
        server.aes_aad(AesAad { transfer_id, aad: aad[7..].to_vec() }, sender).unwrap();
        server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut plaintext),
                    transfer_id,
                    direction: Direction::Encrypt,
                    offset: 0,
                    len: 32,
                },
                sender,
            )
            .unwrap();
        server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut plaintext),
                    transfer_id,
                    direction: Direction::Encrypt,
                    offset: 32,
                    len: plaintext.len() - 32,
                },
                sender,
            )
            .unwrap();

        assert_eq!(plaintext, expected_ciphertext);
        assert_eq!(server.aes_get_tag(AesGcmTag { transfer_id }, sender).unwrap(), expected_tag);
    }

    #[test]
    fn server_enforces_alignment_and_gcm_state_rules() {
        let mut server = CryptoServer::new();
        let sender = pid();

        let ecb_id =
            server.aes_setup(AesSetup { key: vec![0x33; 16], mode: AesMode::Ecb }, sender).unwrap() as u8;
        let mut ecb_buf = b"short".to_vec();
        let ecb_err = server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut ecb_buf),
                    transfer_id: ecb_id,
                    direction: Direction::Encrypt,
                    offset: 0,
                    len: ecb_buf.len(),
                },
                sender,
            )
            .unwrap_err();
        assert_eq!(ecb_err, CryptoError::UnalignedDataLength);

        let ctr_id = server
            .aes_setup(AesSetup { key: vec![0x44; 16], mode: AesMode::Ctr { iv: [0x55; 16] } }, sender)
            .unwrap() as u8;
        let mut ctr_buf = b"partial payload".to_vec();
        server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut ctr_buf),
                    transfer_id: ctr_id,
                    direction: Direction::Encrypt,
                    offset: 0,
                    len: ctr_buf.len(),
                },
                sender,
            )
            .unwrap();
        let ctr_err = server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut ctr_buf),
                    transfer_id: ctr_id,
                    direction: Direction::Encrypt,
                    offset: 0,
                    len: ctr_buf.len(),
                },
                sender,
            )
            .unwrap_err();
        assert_eq!(ctr_err, CryptoError::InvalidState);

        let gcm_id = server
            .aes_setup(AesSetup { key: vec![0x66; 16], mode: AesMode::Gcm { iv: [0x77; 12] } }, sender)
            .unwrap() as u8;
        server.aes_aad(AesAad { transfer_id: gcm_id, aad: b"aad".to_vec() }, sender).unwrap();

        let mut gcm_buf = b"payload".to_vec();
        server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut gcm_buf),
                    transfer_id: gcm_id,
                    direction: Direction::Encrypt,
                    offset: 0,
                    len: gcm_buf.len(),
                },
                sender,
            )
            .unwrap();

        let aad_err =
            server.aes_aad(AesAad { transfer_id: gcm_id, aad: b"late".to_vec() }, sender).unwrap_err();
        assert_eq!(aad_err, CryptoError::InvalidState);

        let tag = server.aes_get_tag(AesGcmTag { transfer_id: gcm_id }, sender).unwrap();
        assert_ne!(tag, [0; 16]);

        let aad_after_tag_err =
            server.aes_aad(AesAad { transfer_id: gcm_id, aad: b"late".to_vec() }, sender).unwrap_err();
        assert_eq!(aad_after_tag_err, CryptoError::InvalidState);

        let second_tag_err = server.aes_get_tag(AesGcmTag { transfer_id: gcm_id }, sender).unwrap_err();
        assert_eq!(second_tag_err, CryptoError::InvalidState);

        let mode_err = server.aes_get_tag(AesGcmTag { transfer_id: ecb_id }, sender).unwrap_err();
        assert_eq!(mode_err, CryptoError::InvalidMode);
    }

    #[test]
    fn gcm_aad_chunking_matches_single_call() {
        let key = vec![0x66; 16];
        let iv = [0x77; 12];
        let aad = b"ab";
        let sender = PID::new(1).unwrap();

        let mut single_server = CryptoServer::new();
        let single_id = single_server
            .aes_setup(AesSetup { key: key.clone(), mode: AesMode::Gcm { iv } }, sender)
            .unwrap() as u8;
        single_server.aes_aad(AesAad { transfer_id: single_id, aad: aad.to_vec() }, sender).unwrap();
        let single_tag = single_server.aes_get_tag(AesGcmTag { transfer_id: single_id }, sender).unwrap();

        let mut split_server = CryptoServer::new();
        let split_id =
            split_server.aes_setup(AesSetup { key, mode: AesMode::Gcm { iv } }, sender).unwrap() as u8;
        split_server.aes_aad(AesAad { transfer_id: split_id, aad: b"a".to_vec() }, sender).unwrap();
        split_server.aes_aad(AesAad { transfer_id: split_id, aad: b"b".to_vec() }, sender).unwrap();
        let split_tag = split_server.aes_get_tag(AesGcmTag { transfer_id: split_id }, sender).unwrap();

        assert_eq!(split_tag, single_tag);
    }

    #[test]
    fn gcm_unaligned_aad_with_data_chunking_matches_single_call() {
        let key = vec![0x66; 16];
        let iv = [0x77; 12];
        let aad = b"ab";
        let payload = b"payload".to_vec();
        let sender = PID::new(1).unwrap();

        let mut single_server = CryptoServer::new();
        let single_id = single_server
            .aes_setup(AesSetup { key: key.clone(), mode: AesMode::Gcm { iv } }, sender)
            .unwrap() as u8;
        single_server.aes_aad(AesAad { transfer_id: single_id, aad: aad.to_vec() }, sender).unwrap();
        let mut single_buf = payload.clone();
        single_server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut single_buf),
                    transfer_id: single_id,
                    direction: Direction::Encrypt,
                    offset: 0,
                    len: single_buf.len(),
                },
                sender,
            )
            .unwrap();
        let single_tag = single_server.aes_get_tag(AesGcmTag { transfer_id: single_id }, sender).unwrap();

        let mut split_server = CryptoServer::new();
        let split_id =
            split_server.aes_setup(AesSetup { key, mode: AesMode::Gcm { iv } }, sender).unwrap() as u8;
        split_server.aes_aad(AesAad { transfer_id: split_id, aad: b"a".to_vec() }, sender).unwrap();
        split_server.aes_aad(AesAad { transfer_id: split_id, aad: b"b".to_vec() }, sender).unwrap();
        let mut split_buf = payload;
        split_server
            .aes_execute(
                AesExecute {
                    buf: mem_range(&mut split_buf),
                    transfer_id: split_id,
                    direction: Direction::Encrypt,
                    offset: 0,
                    len: split_buf.len(),
                },
                sender,
            )
            .unwrap();
        let split_tag = split_server.aes_get_tag(AesGcmTag { transfer_id: split_id }, sender).unwrap();

        assert_eq!(split_buf, single_buf);
        assert_eq!(split_tag, single_tag);
    }
}
