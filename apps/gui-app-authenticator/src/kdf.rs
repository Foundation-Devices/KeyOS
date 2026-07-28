// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use argon2::{Algorithm, Argon2, AssociatedData, ParamsBuilder, Version};
use scrypt::{scrypt, Params as ScryptParams};
use zeroize::Zeroizing;

const MAX_SCRYPT_N: u32 = 65_536;
const MAX_SCRYPT_R: u32 = 8;
const MAX_SCRYPT_P: u32 = 16;
const MAX_SCRYPT_OUT_LEN: usize = 64;

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub enum KdfError {
    #[error("invalid KDF parameters")]
    InvalidParameter,
}

#[derive(Clone)]
pub struct ScryptRequest {
    pub password: Zeroizing<Vec<u8>>,
    pub salt: Zeroizing<Vec<u8>>,
    pub n: u32,
    pub r: u32,
    pub p: u32,
    pub out_len: usize,
}

#[derive(Clone)]
pub struct Argon2idRequest {
    pub password: Zeroizing<Vec<u8>>,
    pub salt: Zeroizing<Vec<u8>>,
    pub secret: Zeroizing<Vec<u8>>,
    pub associated_data: Zeroizing<Vec<u8>>,
    pub mem_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub out_len: usize,
}

pub async fn derive_scrypt(request: ScryptRequest) -> Result<Vec<u8>, KdfError> {
    derive_scrypt_sync(&request)
}

pub async fn derive_argon2id(request: Argon2idRequest) -> Result<Vec<u8>, KdfError> {
    derive_argon2id_sync(&request)
}

fn derive_scrypt_sync(request: &ScryptRequest) -> Result<Vec<u8>, KdfError> {
    if request.out_len == 0 || request.out_len > MAX_SCRYPT_OUT_LEN {
        return Err(KdfError::InvalidParameter);
    }

    if request.n <= 1
        || request.n > MAX_SCRYPT_N
        || request.r == 0
        || request.r > MAX_SCRYPT_R
        || request.p == 0
        || request.p > MAX_SCRYPT_P
    {
        return Err(KdfError::InvalidParameter);
    }

    let Some(log_n) = request.n.checked_ilog2().map(|value| value as u8) else {
        return Err(KdfError::InvalidParameter);
    };
    if (1u32 << log_n) != request.n {
        return Err(KdfError::InvalidParameter);
    }

    let params = ScryptParams::new(log_n, request.r, request.p, request.out_len)
        .map_err(|_| KdfError::InvalidParameter)?;
    let mut output = vec![0u8; request.out_len];
    scrypt(&request.password, &request.salt, &params, &mut output).map_err(|_| KdfError::InvalidParameter)?;
    Ok(output)
}

fn derive_argon2id_sync(request: &Argon2idRequest) -> Result<Vec<u8>, KdfError> {
    let associated_data =
        AssociatedData::new(&request.associated_data).map_err(|_| KdfError::InvalidParameter)?;
    let params = ParamsBuilder::new()
        .m_cost(request.mem_kib)
        .t_cost(request.iterations)
        .p_cost(request.parallelism)
        .output_len(request.out_len)
        .data(associated_data)
        .build()
        .map_err(|_| KdfError::InvalidParameter)?;
    let argon2 = Argon2::new_with_secret(&request.secret, Algorithm::Argon2id, Version::V0x13, params)
        .map_err(|_| KdfError::InvalidParameter)?;

    let mut output = vec![0u8; request.out_len];
    argon2
        .hash_password_into(&request.password, &request.salt, &mut output)
        .map_err(|_| KdfError::InvalidParameter)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use hex_literal::hex;
    use zeroize::Zeroizing;

    use super::{derive_argon2id_sync, derive_scrypt_sync, Argon2idRequest, KdfError, ScryptRequest};

    #[test]
    fn scrypt_matches_rfc7914_empty_vector() {
        let derived = derive_scrypt_sync(&ScryptRequest {
            password: Zeroizing::new(b"".to_vec()),
            salt: Zeroizing::new(b"".to_vec()),
            n: 16,
            r: 1,
            p: 1,
            out_len: 64,
        })
        .unwrap();
        let expected = hex!(
            "
            77 d6 57 62 38 65 7b 20 3b 19 ca 42 c1 8a 04 97
            f1 6b 48 44 e3 07 4a e8 df df fa 3f ed e2 14 42
            fc d0 06 9d ed 09 48 f8 32 6a 75 3a 0f c8 1f 17
            e8 d3 e0 fb 2e 0d 36 28 cf 35 e2 0c 38 d1 89 06
            "
        );
        assert_eq!(derived, expected);
    }

    #[test]
    fn scrypt_matches_rfc7914_password_nacl_vector() {
        let derived = derive_scrypt_sync(&ScryptRequest {
            password: Zeroizing::new(b"password".to_vec()),
            salt: Zeroizing::new(b"NaCl".to_vec()),
            n: 1024,
            r: 8,
            p: 16,
            out_len: 64,
        })
        .unwrap();
        let expected = hex!(
            "
            fd ba be 1c 9d 34 72 00 78 56 e7 19 0d 01 e9 fe
            7c 6a d7 cb c8 23 78 30 e7 73 76 63 4b 37 31 62
            2e af 30 d9 2e 22 a3 88 6f f1 09 27 9d 98 30 da
            c7 27 af b9 4a 83 ee 6d 83 60 cb df a2 cc 06 40
            "
        );
        assert_eq!(derived, expected);
    }

    #[test]
    fn argon2id_matches_rfc9106_section_5_3_vector() {
        let derived = derive_argon2id_sync(&Argon2idRequest {
            password: Zeroizing::new(vec![0x01; 32]),
            salt: Zeroizing::new(vec![0x02; 16]),
            secret: Zeroizing::new(vec![0x03; 8]),
            associated_data: Zeroizing::new(vec![0x04; 12]),
            mem_kib: 32,
            iterations: 3,
            parallelism: 4,
            out_len: 32,
        })
        .unwrap();
        let expected = hex!(
            "
            0d 64 0d f5 8d 78 76 6c 08 c0 37 a3 4a 8b 53 c9
            d0 1e f0 45 2d 75 b6 5e b5 25 20 e9 6b 01 e6 59
            "
        );
        assert_eq!(derived, expected);
    }

    #[test]
    fn scrypt_rejects_r_above_limit() {
        let error = derive_scrypt_sync(&ScryptRequest {
            password: Zeroizing::new(b"password".to_vec()),
            salt: Zeroizing::new(b"NaCl".to_vec()),
            n: 1024,
            r: 9,
            p: 1,
            out_len: 64,
        })
        .unwrap_err();
        assert_eq!(error, KdfError::InvalidParameter);
    }

    #[test]
    fn scrypt_rejects_p_above_limit() {
        let error = derive_scrypt_sync(&ScryptRequest {
            password: Zeroizing::new(b"password".to_vec()),
            salt: Zeroizing::new(b"NaCl".to_vec()),
            n: 1024,
            r: 8,
            p: 17,
            out_len: 64,
        })
        .unwrap_err();
        assert_eq!(error, KdfError::InvalidParameter);
    }
}
