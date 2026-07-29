// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use {
    crate::CryptoApi,
    anyhow::anyhow,
    subtle::ConstantTimeEq,
    xous::{DropDeallocate, MemoryFlags},
    zeroize::Zeroize,
};

pub enum GcmDecryptError {
    AuthenticationFailed,
    Operation(anyhow::Error),
}

pub fn decrypt_gcm(
    crypto: &CryptoApi,
    key: &[u8],
    nonce: [u8; 12],
    expected_tag: [u8; 16],
    ciphertext: &[u8],
    aad: Option<&[u8]>,
) -> Result<Vec<u8>, GcmDecryptError> {
    // Keep the AES-GCM context scoped to a single decrypt operation so the server-side
    // key material and GCM state are cleared immediately when this function returns.
    let aes = crypto
        .setup_aes(key, crypto::AesMode::Gcm { iv: nonce })
        .map_err(|error| GcmDecryptError::Operation(anyhow!("AES setup failed: {error}")))?;
    if let Some(aad) = aad {
        aes.add_aad(aad)
            .map_err(|error| GcmDecryptError::Operation(anyhow!("AES AAD setup failed: {error}")))?;
    }

    let mapped_len = ciphertext.len().next_multiple_of(0x1000).max(0x1000);
    let mut buffer = xous::map_memory(None, None, mapped_len, MemoryFlags::W | MemoryFlags::POPULATE)
        .map(DropDeallocate::new)
        .map_err(|_| GcmDecryptError::Operation(anyhow!("AES execution failed: failed to map AES buffer")))?;
    buffer.as_slice_mut()[..ciphertext.len()].copy_from_slice(ciphertext);

    if let Err(error) = aes.execute(*buffer, 0, ciphertext.len(), crypto::Direction::Decrypt) {
        zeroize_plaintext(&mut buffer, ciphertext.len());
        return Err(GcmDecryptError::Operation(anyhow!("AES execution failed: {error}")));
    }

    let actual_tag = match aes.gcm_tag() {
        Ok(tag) => tag,
        Err(error) => {
            zeroize_plaintext(&mut buffer, ciphertext.len());
            return Err(GcmDecryptError::Operation(anyhow!("AES execution failed: {error}")));
        }
    };
    if !bool::from(actual_tag.ct_eq(&expected_tag)) {
        zeroize_plaintext(&mut buffer, ciphertext.len());
        return Err(GcmDecryptError::AuthenticationFailed);
    }

    let plaintext = buffer.as_slice()[..ciphertext.len()].to_vec();
    zeroize_plaintext(&mut buffer, ciphertext.len());
    Ok(plaintext)
}

fn zeroize_plaintext(buffer: &mut DropDeallocate, plaintext_len: usize) {
    let plaintext: &mut [u8] = &mut buffer.as_slice_mut()[..plaintext_len];
    plaintext.zeroize();
}
