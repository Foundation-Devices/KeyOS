// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use server::{CheckedConn, CheckedPermissions, MessageAllowed};

use crate::{error::NfcError, messages::*};

#[macro_export]
macro_rules! use_api {
    () => {
        mod nfc_permissions {
            use nfc::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/nfc"]
            pub struct NfcPermissions;
        }
        type NfcApi = nfc::api::NfcApi<nfc_permissions::NfcPermissions>;
    };
}

pub struct NfcApi<P: CheckedPermissions> {
    conn: CheckedConn<P>,
    buf: xous_ipc::Buffer<'static>,
}

impl<P: CheckedPermissions> Default for NfcApi<P> {
    fn default() -> Self { Self { conn: Default::default(), buf: xous_ipc::Buffer::new(872) } }
}

impl<P: CheckedPermissions> NfcApi<P> {
    pub fn read_ndef_raw_msg(&mut self, timeout: Duration) -> Result<(Vec<u8>, Vec<u8>), NfcError>
    where
        P: MessageAllowed<ReadNdefRawMsg>,
    {
        self.conn.send_blocking_archive_buf(&mut self.buf, ReadNdefRawMsg(timeout))
    }

    pub fn write_ndef_raw_msg(
        &mut self,
        uid: Vec<u8>,
        msg: Vec<u8>,
        timeout: Duration,
    ) -> Result<(), NfcError>
    where
        P: MessageAllowed<WriteNdefRawMsg>,
    {
        self.conn.send_blocking_archive_buf(&mut self.buf, WriteNdefRawMsg((uid, msg, timeout)))
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), NfcError>
    where
        P: MessageAllowed<SetEnabled>,
    {
        Ok(self.conn.try_send_blocking_scalar(SetEnabled(enabled))?)
    }

    pub fn is_enabled(&self) -> Result<bool, NfcError>
    where
        P: MessageAllowed<IsEnabled>,
    {
        Ok(self.conn.try_send_blocking_scalar(IsEnabled)?)
    }

    pub fn is_active(&self) -> Result<bool, NfcError>
    where
        P: MessageAllowed<IsActive>,
    {
        Ok(self.conn.try_send_blocking_scalar(IsActive)?)
    }
}
