// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod authenticate;
mod register;
mod version;

pub use authenticate::{AuthenticateRequest, AuthenticateResponse};
pub use register::{RegisterRequest, RegisterResponse};
pub use version::VersionResponse;

use super::error::Error;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    Register,
    Authenticate,
    Version,
}
impl TryFrom<u8> for Command {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0x01 => Ok(Command::Register),
            0x02 => Ok(Command::Authenticate),
            0x03 => Ok(Command::Version),
            v => Err(Error::InstructionNotSupported(v)),
        }
    }
}

#[derive(Debug)]
pub struct KeyHandle {
    pub security_key_index: usize,
    pub registered_key_index: usize,
}
impl KeyHandle {
    /// Serialize as 8 bytes: two big-endian `u32`s. Explicit width so the on-wire format is the
    /// same on 32-bit ARM (the real device) and on 64-bit hosts (simulator / unit tests), even
    /// though `security_key_index`/`registered_key_index` are declared as `usize` elsewhere.
    pub(crate) fn to_vec(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(8);
        data.extend_from_slice(&(self.security_key_index as u32).to_be_bytes());
        data.extend_from_slice(&(self.registered_key_index as u32).to_be_bytes());
        data
    }

    pub(crate) fn from_bytes(data: &[u8]) -> Result<Self, Error> {
        let bytes: &[u8; 8] = data.try_into().map_err(|_| Error::WrongData)?;
        Ok(Self {
            security_key_index: u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize,
            registered_key_index: u32::from_be_bytes(bytes[4..].try_into().unwrap()) as usize,
        })
    }
}
