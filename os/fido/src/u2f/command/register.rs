// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{Error, KeyHandle};

#[derive(Debug)]
pub struct RegisterRequest {
    pub challenge_parameter: [u8; 32],
    pub application_parameter: [u8; 32],
}
impl RegisterRequest {
    pub fn from_apdu_data(data: &[u8]) -> Result<RegisterRequest, Error> {
        if data.len() != 64 {
            return Err(Error::WrongLength);
        }
        Ok(RegisterRequest {
            challenge_parameter: data[..32].try_into().unwrap(),
            application_parameter: data[32..64].try_into().unwrap(),
        })
    }
}

#[derive(Debug)]
pub struct RegisterResponse {
    pub user_public_key: Vec<u8>,
    pub key_handle: KeyHandle,
    pub attestation_certificate: Vec<u8>,
    pub attestation_signature: Vec<u8>,
}
impl RegisterResponse {
    pub fn new(
        user_public_key: Vec<u8>,
        key_handle: KeyHandle,
        attestation_certificate: Vec<u8>,
    ) -> RegisterResponse {
        RegisterResponse {
            user_public_key,
            key_handle,
            attestation_certificate,
            attestation_signature: Vec::new(),
        }
    }

    pub fn attest(&mut self, application_parameter: &[u8], challenge_parameter: &[u8]) -> Result<(), Error> {
        let mut signature_base = vec![0x00];
        signature_base.extend_from_slice(application_parameter);
        signature_base.extend_from_slice(challenge_parameter);
        signature_base.extend_from_slice(&self.key_handle.to_vec());
        signature_base.extend_from_slice(&self.user_public_key);
        log::debug!("Attestation Signature base: {:02x?}", signature_base);
        let signature_base_hash =
            crate::CryptoApi::default().sha256(&signature_base).map_err(|_| Error::Hashing)?;
        // Always sign with the SE - the certificate was generated during init
        let sig =
            crate::Security::default().sign_with_fido_key(signature_base_hash).map_err(|_| Error::Signing)?;
        let sig = p256::ecdsa::Signature::from_slice(&sig)
            .inspect_err(|e| log::error!("Signature::from_slice {e:?}"))
            .map_err(|_| Error::Signing)?;
        self.attestation_signature = sig.to_der().as_bytes().to_vec();
        Ok(())
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut data = vec![0x05];
        data.extend_from_slice(&self.user_public_key);
        let key_handle = self.key_handle.to_vec();
        data.push(key_handle.len() as u8);
        data.extend_from_slice(&key_handle);
        data.extend_from_slice(&self.attestation_certificate);
        data.extend_from_slice(&self.attestation_signature);
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_register_request() {
        let data = [
            0x7e, 0xa6, 0x52, 0xb3, 0x29, 0xaf, 0xe7, 0xaf, 0x23, 0x0f, 0x45, 0xea, 0xaa, 0xae, 0x6f, 0x98,
            0x47, 0x38, 0xaa, 0xf0, 0xa2, 0x76, 0xca, 0xa8, 0xf0, 0x99, 0x28, 0x3b, 0x8d, 0xf6, 0x65, 0x3b,
            0x20, 0xa8, 0x3b, 0x42, 0xcd, 0x54, 0x0c, 0x2f, 0xcd, 0xee, 0x61, 0xe4, 0xf1, 0x47, 0x93, 0xed,
            0xe7, 0x74, 0xfa, 0x82, 0x58, 0x83, 0x56, 0x83, 0xa7, 0xc8, 0xe8, 0x85, 0xa8, 0xc2, 0x70, 0xc2,
        ];
        let req = RegisterRequest::from_apdu_data(&data);
        println!("{:02x?}", req);
        assert!(req.is_ok());
    }
}
