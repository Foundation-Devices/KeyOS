// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use crypto::{error::CryptoError, SHA256_HASH_SIZE};
use security::{PinError, Seed, MIN_PIN_LENGTH};

#[cfg(keyos)]
mod atsama5d2;
#[cfg(keyos)]
mod config;
#[cfg(not(keyos))]
mod hosted;
#[cfg(keyos)]
mod se_port;

#[cfg(keyos)]
use atsama5d2::Server;
#[cfg(not(keyos))]
use hosted::Server;

crypto::use_api!();

/// Validates a raw PIN string according to security requirements
fn validate_raw_pin(raw_pin: &str) -> Result<(), PinError> {
    if raw_pin.len() < MIN_PIN_LENGTH {
        return Err(PinError::TooShort);
    }
    Ok(())
}

pub(crate) fn seed_fingerprint(
    crypto: &CryptoApi,
    seed: &Seed,
) -> Result<[u8; SHA256_HASH_SIZE], CryptoError> {
    sha256_batch(crypto, &[seed.bytes(), b"Fingerprint"])
}

pub(crate) fn sha256_batch(
    crypto: &CryptoApi,
    batch: &[&[u8]],
) -> Result<[u8; SHA256_HASH_SIZE], CryptoError> {
    let mut ctx = crypto.sha256_init();
    for slice in batch {
        ctx.update(slice)?;
    }
    ctx.finalize()?.try_into().map_err(|_| CryptoError::InvalidDataLength)
}

#[allow(dead_code)]
pub(crate) fn sha256(crypto: &CryptoApi, input: &[u8]) -> Result<[u8; SHA256_HASH_SIZE], CryptoError> {
    crypto.sha256(input)
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System7).unwrap();

    log::info!("Security server pid: {}", server::xous::current_pid().unwrap());
    server::listen(Server::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pin_validation_too_short() {
        // Test various short PINs
        assert!(matches!(validate_raw_pin(""), Err(PinError::TooShort)));
        assert!(matches!(validate_raw_pin("1"), Err(PinError::TooShort)));
        assert!(matches!(validate_raw_pin("12"), Err(PinError::TooShort)));
        assert!(matches!(validate_raw_pin("123"), Err(PinError::TooShort)));
        assert!(matches!(validate_raw_pin("1234"), Err(PinError::TooShort)));
        assert!(matches!(validate_raw_pin("12345"), Err(PinError::TooShort)));
    }

    #[test]
    fn test_pin_validation_valid_length() {
        // Test minimum valid length
        assert!(validate_raw_pin("123456").is_ok());

        // Test longer PINs
        assert!(validate_raw_pin("1234567").is_ok());
        assert!(validate_raw_pin("12345678").is_ok());
        assert!(validate_raw_pin("123456789012345678901234567890").is_ok());
    }

    #[test]
    fn test_pin_validation_boundary() {
        // Test exactly at the boundary
        let min_length_pin = "1".repeat(MIN_PIN_LENGTH);
        let too_short_pin = "1".repeat(MIN_PIN_LENGTH - 1);

        assert!(validate_raw_pin(&min_length_pin).is_ok());
        assert!(matches!(validate_raw_pin(&too_short_pin), Err(PinError::TooShort)));
    }
}
