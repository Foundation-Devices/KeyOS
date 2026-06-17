// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{Error, KeyHandle};

#[derive(Debug)]
pub struct AuthenticateRequest {
    pub challenge_parameter: [u8; 32],
    pub application_parameter: [u8; 32],
    pub key_handle: KeyHandle,
}
impl AuthenticateRequest {
    pub fn from_apdu_data(data: &[u8]) -> Result<AuthenticateRequest, Error> {
        if data.len() <= 64 {
            return Err(Error::WrongLength);
        }
        // Per FIDO U2F §5.4 a key handle can be 1..=255 bytes — any length produced by any
        // authenticator is spec-legal. We accept any length here and let `KeyHandle::from_bytes`
        // reject handles that don't match our own 8-byte format (returning SW_WRONG_DATA, which
        // RPs interpret as "not my key, try the next one"). Rejecting with SW_WRONG_LENGTH would
        // trip some RPs' multi-key check-only sweep and abort the whole operation.
        let key_handle_len = data[64] as usize;
        if data.len() != 64 + 1 + key_handle_len {
            return Err(Error::WrongLength);
        }
        Ok(AuthenticateRequest {
            challenge_parameter: data[..32].try_into().unwrap(),
            application_parameter: data[32..64].try_into().unwrap(),
            key_handle: KeyHandle::from_bytes(&data[64 + 1..64 + 1 + key_handle_len])?,
        })
    }
}

#[derive(Debug)]
pub struct AuthenticateResponse {
    pub user_presence: u8,
    pub counter: u32,
    pub signature: Vec<u8>,
}
impl AuthenticateResponse {
    pub fn new(user_present: bool) -> AuthenticateResponse {
        AuthenticateResponse {
            user_presence: if user_present { 1 } else { 0 },
            counter: 0,
            signature: Vec::new(),
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut data = vec![self.user_presence];
        data.extend_from_slice(&self.counter.to_be_bytes());
        data.extend_from_slice(&self.signature);
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build the decoded APDU data field consumed by `from_apdu_data`.
    fn authenticate_body(challenge: &[u8; 32], app_param: &[u8; 32], key_handle: &[u8]) -> Vec<u8> {
        let khl = key_handle.len();
        let data_len = 64 + 1 + khl;
        let mut v = Vec::with_capacity(data_len);
        v.extend_from_slice(challenge);
        v.extend_from_slice(app_param);
        v.push(khl as u8);
        v.extend_from_slice(key_handle);
        v
    }

    #[test]
    fn decode_authenticate_request_short_payload_returns_wrong_length() {
        // Payload too short to even contain challenge + app_param.
        let body = [0x01u8; 40];
        assert_eq!(AuthenticateRequest::from_apdu_data(&body).unwrap_err(), Error::WrongLength);
    }

    #[test]
    fn decode_authenticate_request_declared_lc_mismatch_returns_wrong_length() {
        // Declared key-handle length doesn't match the remaining data.
        let mut body =
            authenticate_body(&[0x77; 32], &[0x88; 32], &[0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00]);
        body[64] = 99; // lie about key handle length
        assert_eq!(AuthenticateRequest::from_apdu_data(&body).unwrap_err(), Error::WrongLength);
    }

    #[test]
    fn decode_authenticate_request_our_8_byte_handle() {
        // Golden path: our own 8-byte (security_key_index, registered_key_index) handle parses.
        let challenge = [0x11u8; 32];
        let app_param = [0x22u8; 32];
        let key_handle = [0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00];
        let body = authenticate_body(&challenge, &app_param, &key_handle);
        let req = AuthenticateRequest::from_apdu_data(&body).expect("should parse");
        assert_eq!(req.challenge_parameter, challenge);
        assert_eq!(req.application_parameter, app_param);
        assert_eq!(req.key_handle.security_key_index, 5);
        assert_eq!(req.key_handle.registered_key_index, 0);
    }

    #[test]
    fn decode_authenticate_request_foreign_key_handle_returns_wrong_data() {
        // Well-formed short-form APDU carrying a 64-byte foreign key handle (YubiKey-shaped).
        // Before SFT-6918 this returned `WrongLength` (SW_6700). Now it must return `WrongData`
        // (SW_6A80) so the RP's multi-key probe moves to the next credential.
        let body = authenticate_body(&[0x33; 32], &[0x44; 32], &[0xAA; 64]);
        assert_eq!(AuthenticateRequest::from_apdu_data(&body).unwrap_err(), Error::WrongData);
    }

    #[test]
    fn decode_authenticate_request_one_byte_handle_returns_wrong_data() {
        // Spec-legal minimum (1 byte) handle that isn't ours — also WrongData, not WrongLength.
        let body = authenticate_body(&[0x55; 32], &[0x66; 32], &[0xCC]);
        assert_eq!(AuthenticateRequest::from_apdu_data(&body).unwrap_err(), Error::WrongData);
    }
}
