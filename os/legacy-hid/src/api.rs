// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{CheckedConn, CheckedPermissions, MessageAllowed};

#[cfg(keyos)]
use crate::messages::DeliverHidApdu;
use crate::messages::{SetLegacyMode, WriteHidApdu};

#[macro_export]
macro_rules! use_api {
    () => {
        mod legacy_hid_permissions {
            use legacy_hid::messages::*;
            #[derive(Debug, Clone, Default, server::Permissions)]
            #[server_name = "os/legacy-hid"]
            pub struct LegacyHidPermissions;
        }
        type LegacyHidApi = legacy_hid::api::LegacyHidApi<legacy_hid_permissions::LegacyHidPermissions>;
    };
}

#[derive(Debug, Default)]
pub struct LegacyHidApi<P: CheckedPermissions>(CheckedConn<P>);

impl<P: CheckedPermissions> LegacyHidApi<P> {
    /// Send an outgoing APDU back to the host. Fire-and-forget — the server
    /// fragments and writes it to the IN endpoint.
    pub fn write_apdu(&self, channel_id: u16, data: Vec<u8>) -> Result<(), xous::Error>
    where
        P: MessageAllowed<WriteHidApdu>,
    {
        self.0.try_send_archive(WriteHidApdu { channel_id, data })
    }

    /// Toggle the Legacy USB identity. Blocks until the controller reset finishes.
    pub fn set_legacy_mode(&self, active: bool)
    where
        P: MessageAllowed<SetLegacyMode>,
    {
        let _ = self.0.try_send_blocking_scalar(SetLegacyMode(active));
    }

    /// Internal: the OUT thread uses this to deliver a reassembled APDU back
    /// into the server's main loop. Permission is only granted to legacy-hid
    /// itself.
    #[cfg(keyos)]
    pub(crate) fn deliver_hid_apdu(&self, channel_id: u16, data: Vec<u8>) -> Result<(), xous::Error>
    where
        P: MessageAllowed<DeliverHidApdu>,
    {
        self.0.try_send_archive(DeliverHidApdu { channel_id, data })
    }
}
