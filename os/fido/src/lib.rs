// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod api;
mod attestation_cert;
mod ctap;
pub mod error;
mod implementation;
pub mod messages;
mod nav_thread;
mod u2f;

use ctap::{PublicKeyCredentialRpEntity, PublicKeyCredentialUserEntity};
use error::FidoError;

security::use_api!();

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegisteredKeyU2f {
    pub application_parameter: [u8; 32],
    pub signature_counter: u32,
    pub registered_timestamp: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegisteredKeyCtap {
    pub rp: PublicKeyCredentialRpEntity,
    pub user: PublicKeyCredentialUserEntity,
    pub signature_counter: u32,
    pub registered_timestamp: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RegisteredKey {
    U2f(RegisteredKeyU2f),
    Ctap(RegisteredKeyCtap),
}

crypto::use_api!();

impl RegisteredKey {
    pub(crate) fn signature_counter(&self) -> u32 {
        match self {
            RegisteredKey::U2f(key) => key.signature_counter,
            RegisteredKey::Ctap(key) => key.signature_counter,
        }
    }

    pub(crate) fn inc_signature_counter(&mut self) -> u32 {
        match self {
            RegisteredKey::U2f(key) => {
                key.signature_counter += 1;
                key.signature_counter
            }
            RegisteredKey::Ctap(key) => {
                key.signature_counter += 1;
                key.signature_counter
            }
        }
    }
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecurityKey {
    pub registered_keys: Vec<RegisteredKey>,
    pub live: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub color: u8,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub date: u64,
}

/// View of a SecurityKey exposed to UI clients via IPC.
/// Must be serializable with rkyv for archive events/messages.
#[derive(
    Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, serde::Serialize, serde::Deserialize,
)]
pub struct SecurityKeyView {
    pub index: usize,
    pub label: String,
    pub color: u8,
    pub icon: String,
    pub archived: bool,
    pub live: bool,
    pub date: u64,
    pub registered_count: usize,
}

impl SecurityKey {
    /// Convert to a view suitable for IPC, given the key's index.
    pub fn to_view(&self, index: usize) -> SecurityKeyView {
        SecurityKeyView {
            index,
            label: self.label.clone(),
            color: self.color,
            icon: self.icon.clone(),
            archived: self.archived,
            live: self.live,
            date: self.date,
            registered_count: self.registered_keys.len(),
        }
    }

    /// Set archived state. Archived keys are automatically not live.
    pub fn set_archived(&mut self, archived: bool) {
        self.archived = archived;
        if archived {
            self.live = false;
        } else {
            self.live = true;
        }
    }
}

impl SecurityKey {
    fn registered_key(&self, index: usize) -> Result<&RegisteredKey, FidoError> {
        self.registered_keys.get(index).ok_or(FidoError::InvalidIndex)
    }

    fn registered_key_mut(&mut self, index: usize) -> Result<&mut RegisteredKey, FidoError> {
        self.registered_keys.get_mut(index).ok_or(FidoError::InvalidIndex)
    }

    fn registered_key_indexes(&self, u2f: bool, tag: &[u8]) -> Vec<usize> {
        self.registered_keys
            .iter()
            .enumerate()
            .filter_map(|(index, key)| match (u2f, key) {
                (true, RegisteredKey::U2f(key)) if key.application_parameter == tag => Some(index),
                (false, RegisteredKey::Ctap(key)) if key.rp.id.as_bytes() == tag => Some(index),
                _ => None,
            })
            .collect()
    }
}

pub fn listen() {
    let (security, app_seed) = implementation::wait();
    server::listen(implementation::FidoServer::new(security, app_seed).unwrap())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn inc_signature_counter() {
        let mut key = RegisteredKey::U2f(RegisteredKeyU2f {
            application_parameter: [1; 32],
            signature_counter: 0,
            registered_timestamp: Utc::now().timestamp() as u32,
        });
        assert_eq!(key.signature_counter(), 0);
        assert_eq!(key.inc_signature_counter(), 1);
        assert_eq!(key.signature_counter(), 1);
    }

    #[test]
    fn registered_key_indexes() {
        let security_key = SecurityKey {
            registered_keys: vec![
                RegisteredKey::Ctap(RegisteredKeyCtap {
                    rp: PublicKeyCredentialRpEntity { id: "test.com".to_string(), name: None },
                    user: PublicKeyCredentialUserEntity {
                        id: vec![12],
                        name: None,
                        display_name: None,
                        icon: None,
                    },
                    signature_counter: 0,
                    registered_timestamp: Utc::now().timestamp() as u32,
                }),
                RegisteredKey::Ctap(RegisteredKeyCtap {
                    rp: PublicKeyCredentialRpEntity { id: "test2.com".to_string(), name: None },
                    user: PublicKeyCredentialUserEntity {
                        id: vec![13],
                        name: None,
                        display_name: None,
                        icon: None,
                    },
                    signature_counter: 1,
                    registered_timestamp: Utc::now().timestamp() as u32,
                }),
                RegisteredKey::Ctap(RegisteredKeyCtap {
                    rp: PublicKeyCredentialRpEntity { id: "test.com".to_string(), name: None },
                    user: PublicKeyCredentialUserEntity {
                        id: vec![14],
                        name: None,
                        display_name: None,
                        icon: None,
                    },
                    signature_counter: 0,
                    registered_timestamp: Utc::now().timestamp() as u32,
                }),
                RegisteredKey::U2f(RegisteredKeyU2f {
                    application_parameter: [1; 32],
                    signature_counter: 0,
                    registered_timestamp: Utc::now().timestamp() as u32,
                }),
                RegisteredKey::U2f(RegisteredKeyU2f {
                    application_parameter: [2; 32],
                    signature_counter: 0,
                    registered_timestamp: Utc::now().timestamp() as u32,
                }),
                RegisteredKey::U2f(RegisteredKeyU2f {
                    application_parameter: [1; 32],
                    signature_counter: 0,
                    registered_timestamp: Utc::now().timestamp() as u32,
                }),
            ],
            live: false,
            ..Default::default()
        };
        let registered_key_indexes_ctap = security_key.registered_key_indexes(false, "test.com".as_bytes());
        println!("{:?}", registered_key_indexes_ctap);
        assert_eq!(registered_key_indexes_ctap, vec![0, 2]);
        let registered_key_indexes_u2f = security_key.registered_key_indexes(true, &[1; 32]);
        println!("{:?}", registered_key_indexes_u2f);
        assert_eq!(registered_key_indexes_u2f, vec![3, 5]);
    }
}
