// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use foundation_api::{
    bc_components::{self},
    bc_xid::XIDDocument,
    dcbor::{self, CBORCase, CBOR},
    quantum_link::QuantumLinkIdentity,
};

use crate::persist::{self, FileBacked};

#[derive(Debug, PartialEq, Eq)]
pub struct QuantumLinkState {
    pub system_identity: SystemIdentity,
    pub paired_device: Option<PairedDevice>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct PairedDevice {
    pub xid: XIDDocument,
    pub name: String,
}

impl TryFrom<CBOR> for PairedDevice {
    type Error = dcbor::Error;

    fn try_from(value: CBOR) -> Result<Self, Self::Error> {
        let case = value.into_case();

        let CBORCase::Map(map) = case else {
            return Err(dcbor::Error::WrongType);
        };

        Ok(PairedDevice {
            xid: map.get("xid").ok_or(dcbor::Error::MissingMapKey)?,
            name: map.get("name").ok_or(dcbor::Error::MissingMapKey)?,
        })
    }
}

impl From<PairedDevice> for CBOR {
    fn from(value: PairedDevice) -> Self {
        let mut map = dcbor::Map::new();
        map.insert(CBOR::from("xid"), value.xid.clone());
        map.insert(CBOR::from("name"), value.name.clone());
        CBOR::from(map)
    }
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct AppId(pub xous::AppId);

impl From<AppId> for CBOR {
    fn from(value: AppId) -> Self {
        let bytes = dcbor::ByteString::new(value.0 .0);
        CBOR::from(bytes)
    }
}

impl TryFrom<CBOR> for AppId {
    type Error = dcbor::Error;

    fn try_from(value: CBOR) -> Result<Self, Self::Error> {
        use xous::APP_ID_SIZE;

        let case = value.into_case();
        let CBORCase::ByteString(value) = case else {
            return Err(dcbor::Error::WrongType);
        };

        if value.data().len() != APP_ID_SIZE {
            return Err(dcbor::Error::WrongType);
        }

        let mut app_id = [0; APP_ID_SIZE];
        app_id.copy_from_slice(value.data());

        Ok(AppId(xous::AppId(app_id)))
    }
}

impl std::fmt::Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 .0 {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SystemIdentity {
    pub xid_document: XIDDocument,
    pub private_keys: bc_components::PrivateKeys,
}

impl TryFrom<CBOR> for SystemIdentity {
    type Error = dcbor::Error;

    fn try_from(value: CBOR) -> Result<Self, Self::Error> {
        let case = value.into_case();

        let CBORCase::Map(map) = case else {
            return Err(dcbor::Error::WrongType);
        };

        Ok(SystemIdentity {
            xid_document: map.get("xid_document").ok_or(dcbor::Error::MissingMapKey)?,
            private_keys: map.get("private_keys").ok_or(dcbor::Error::MissingMapKey)?,
        })
    }
}

impl From<SystemIdentity> for CBOR {
    fn from(value: SystemIdentity) -> Self {
        let mut map = dcbor::Map::new();
        map.insert(CBOR::from("xid_document"), value.xid_document.clone());
        map.insert(CBOR::from("private_keys"), value.private_keys.clone());
        CBOR::from(map)
    }
}

impl QuantumLinkState {
    pub fn new() -> FileBacked<Self> {
        FileBacked::get_or_init("quantum-link-state.cbor".to_string(), || Ok(new_state()))
            .expect("Failed to load or initialize state")
    }
}

impl persist::Persister for QuantumLinkState {
    type Error = anyhow::Error;

    fn from_bytes(data: &[u8]) -> Result<Self, Self::Error> {
        let cbor = CBOR::try_from_data(data).ok().ok_or_else(|| anyhow::anyhow!("Invalid CBOR"))?;
        let case = cbor.into_case();

        let CBORCase::Map(map) = case else {
            return Err(anyhow::anyhow!("Invalid CBOR case"));
        };

        let paired_device: Option<PairedDevice> = map.get("paired_device");
        log::info!("QuantumLink state restored from file. paired_device found {}", paired_device.is_some());

        Ok(QuantumLinkState {
            system_identity: map
                .get("system_identity")
                .ok_or_else(|| anyhow::anyhow!("system_identity not found"))?,
            paired_device,
        })
    }

    fn to_bytes(&self) -> Result<Vec<u8>, Self::Error> {
        let mut map = dcbor::Map::new();
        map.insert("system_identity", self.system_identity.clone());
        if let Some(paired_device) = &self.paired_device {
            map.insert("paired_device", paired_device.clone());
        }
        Ok(CBOR::from(map).to_cbor_data())
    }
}

fn new_state() -> QuantumLinkState {
    let identity = QuantumLinkIdentity::generate();
    log::info!("Initializing QuantumLink state");
    QuantumLinkState {
        system_identity: SystemIdentity {
            xid_document: identity.xid_document,
            private_keys: identity.private_keys.unwrap(),
        },
        paired_device: None,
    }
}

#[test]
fn persist_state() {
    let identity = QuantumLinkIdentity::generate();

    let mut state = new_state();
    state.paired_device = Some(PairedDevice { xid: identity.xid_document, name: String::from("iphone") });

    let cbor = persist::Persister::to_bytes(&state).unwrap();
    let decoded = persist::Persister::from_bytes(&cbor).unwrap();
    assert_eq!(state, decoded);
}
