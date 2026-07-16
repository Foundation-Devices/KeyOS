// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crypto::messages::*;
use hmac::Mac;
use server::xous::PID;
use sha2::{Sha224, Sha256, Sha384, Sha512};

use crate::{CryptoError, ShaAlgo};

#[derive(server::Server)]
#[name = "os/crypto"]
pub struct CryptoServer;

impl server::Server for CryptoServer {}

impl Default for CryptoServer {
    fn default() -> Self { Self }
}

impl CryptoServer {
    pub fn new() -> Self { Self }

    pub fn aes_setup(&mut self, _msg: AesSetup, _sender: PID) -> Result<usize, CryptoError> { Ok(0) }

    pub fn aes_execute(&mut self, msg: AesExecute, _sender: PID) -> Result<usize, CryptoError> { Ok(msg.len) }

    pub fn aes_aad(&mut self, msg: AesAad, _sender: PID) -> Result<usize, CryptoError> { Ok(msg.aad.len()) }

    pub fn aes_get_tag(&mut self, _msg: AesGcmTag, _sender: PID) -> Result<[u8; 16], CryptoError> {
        Ok([0; 16])
    }

    pub fn aes_clear(&mut self, _id: AesClear, _sender: PID) {}

    pub fn hmac(&self, algo: ShaAlgo, key: &[u8], msg: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Ok(match algo {
            ShaAlgo::Sha224 => {
                type HmacSha224 = hmac::Hmac<Sha224>;
                let mut mac = HmacSha224::new_from_slice(key).map_err(|_| CryptoError::InvalidParameter)?;
                mac.update(msg);
                mac.finalize().into_bytes().to_vec()
            }
            ShaAlgo::Sha256 => {
                type HmacSha256 = hmac::Hmac<Sha256>;
                let mut mac = HmacSha256::new_from_slice(key).map_err(|_| CryptoError::InvalidParameter)?;
                mac.update(msg);
                mac.finalize().into_bytes().to_vec()
            }
            ShaAlgo::Sha384 => {
                type HmacSha384 = hmac::Hmac<Sha384>;
                let mut mac = HmacSha384::new_from_slice(key).map_err(|_| CryptoError::InvalidParameter)?;
                mac.update(msg);
                mac.finalize().into_bytes().to_vec()
            }
            ShaAlgo::Sha512 => {
                type HmacSha512 = hmac::Hmac<Sha512>;
                let mut mac = HmacSha512::new_from_slice(key).map_err(|_| CryptoError::InvalidParameter)?;
                mac.update(msg);
                mac.finalize().into_bytes().to_vec()
            }
        })
    }
}
