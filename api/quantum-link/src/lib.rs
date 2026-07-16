// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub use foundation_api;
pub use worker::*;

pub mod messages;
mod worker;

use messages::*;
use server::{CheckedConn, CheckedPermissions, MessageAllowed};

#[macro_export]
macro_rules! use_api {
    () => {
        mod quantum_link_permissions {
            use quantum_link::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/quantum-link"]
            pub struct QuantumLinkPermissions;
        }
        type QuantumLinkApi = quantum_link::QuantumLinkApi<quantum_link_permissions::QuantumLinkPermissions>;
        type QlStatus = quantum_link::QlStatus<quantum_link_permissions::QuantumLinkPermissions>;
    };
}

#[macro_export]
macro_rules! use_prestart_api {
    () => {
        mod quantum_link_prestart_permissions {
            use quantum_link::messages::StartWithoutFilesystem;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/ql-prestart"]
            pub struct QuantumLinkPrestartPermissions;
        }
        use quantum_link_prestart_permissions::QuantumLinkPrestartPermissions;
    };
}

#[derive(Default)]
pub struct QuantumLinkApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
}

impl<P: CheckedPermissions> QuantumLinkApi<P> {
    /// Returns the devices XID document
    pub fn xid_document(&self) -> Vec<u8>
    where
        P: MessageAllowed<GetXidDocument>,
    {
        self.conn.send_blocking_archive(GetXidDocument)
    }

    pub fn subscribe_restore_magic_backup<S>(&self, context: &mut server::ServerContext<S>)
    where
        S: server::Server + server::ArchiveEventHandler<foundation_api::backup::RestoreMagicBackupEvent>,
        P: MessageAllowed<SubscribeRestoreMagicBackup>,
    {
        self.conn.subscribe_archive_infallible(SubscribeRestoreMagicBackup, context)
    }

    pub fn subscribe_firmware_fetch<S>(&self, context: &mut server::ServerContext<S>)
    where
        S: server::Server + server::ArchiveEventHandler<foundation_api::firmware::FirmwareFetchEvent>,
        P: MessageAllowed<SubscribeFirmwareFetch>,
    {
        self.conn.subscribe_archive_infallible(SubscribeFirmwareFetch, context)
    }

    pub fn start_firmware_update(&self, chunk_offset: Option<u64>) -> Result<(), SendMessageError>
    where
        P: MessageAllowed<StartFirmwareUpdate>,
    {
        self.conn.send_blocking_archive(StartFirmwareUpdate { chunk_offset })
    }
}

pub fn start_quantum_link_without_filesystem<P>()
where
    P: CheckedPermissions,
    P: MessageAllowed<StartWithoutFilesystem>,
{
    let Some(conn) =
        server::CheckedConn::<P>::try_connect_with_timeout(std::time::Duration::from_millis(2000))
    else {
        log::warn!("QL prestart server was not running");
        return;
    };
    conn.send_blocking_scalar(StartWithoutFilesystem);
}

#[derive(Debug, Copy, Clone, thiserror::Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum SendMessageError {
    #[error("no device paired")]
    NoDevicePaired,
    #[error(transparent)]
    Bluetooth(#[from] bt::BluetoothError),
    #[error("send message cancelled")]
    Cancelled,
    #[error("timed out")]
    Timeout,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum SecurityCheckState {
    /// Challenge received from Envoy, processing with Security server
    ReceivedChallenge,

    /// Security check completed successfully
    Success,

    /// Security check failed - device validation failed
    Failed,

    /// Error communicating with Foundation servers
    Error,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum PairingEvent {
    RequestReceived,
    PairingComplete { device_name: String, new: bool },
    PairingFailed,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ConnectionStatus {
    pub bt_connected: bool,
    pub ql_paired: bool,
    pub live: bool,
}

impl server::FromScalar<1> for ConnectionStatus {
    fn from_scalar([value]: [u32; 1]) -> Self {
        Self { bt_connected: (value & 0x1) != 0, ql_paired: (value & 0x2) != 0, live: (value & 0x4) != 0 }
    }
}

impl server::AsScalar<1> for ConnectionStatus {
    fn as_scalar(&self) -> [u32; 1] {
        let mut value = 0u32;
        if self.bt_connected {
            value |= 0x1;
        }
        if self.ql_paired {
            value |= 0x2;
        }
        if self.live {
            value |= 0x4;
        }
        [value]
    }
}
