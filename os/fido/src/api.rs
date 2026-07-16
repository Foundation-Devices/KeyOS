// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use server::{CheckedConn, CheckedPermissions, MessageAllowed};

#[cfg(feature = "test-app")]
use crate::messages::ResetState;
use crate::messages::{
    CreateSecurityKey, CtapProcessCbor, EditSecurityKey, GetSelectedSecurityKey, ListSecurityKeys,
    SelectSecurityKey, SetArchived, Transport, U2fProcessApdu,
};
use crate::SecurityKeyView;

#[macro_export]
macro_rules! use_api {
    () => {
        mod fido_permissions {
            use fido::messages::*;
            #[derive(Debug, Clone, Default, server::Permissions)]
            #[server_name = "os/fido"]
            pub struct FidoPermissions;
        }
        type FidoApi = fido::api::FidoApi<fido_permissions::FidoPermissions>;
    };
}

#[derive(Debug, Default)]
pub struct FidoApi<P: CheckedPermissions>(CheckedConn<P>);

impl<P: CheckedPermissions> FidoApi<P> {
    /* API for gui-app-security-keys application */
    // Note: To subscribe to key changes, use:
    //   slint_keyos_platform::subscribe_archive::<FidoPermissions, _>(SubscribeKeyChanges)
    // This returns an async stream of KeysChangedEvent.

    /// Create a new Security Key with metadata. Returns the new key index, or an error if
    /// creation failed.
    pub fn create_security_key(
        &self,
        label: String,
        color: u8,
        icon: String,
    ) -> Result<usize, crate::error::FidoError>
    where
        P: MessageAllowed<CreateSecurityKey>,
    {
        self.0.send_blocking_archive(CreateSecurityKey { label, color, icon })
    }

    /// Edit a Security Key's metadata. Blocks until the server returns the validation
    /// outcome (`EmptyLabel` / `DuplicateLabel` are surfaced here so callers don't need a
    /// separate `validate_label` round-trip). Pass `date: 0` to leave the date unchanged.
    pub fn edit_security_key(
        &self,
        index: usize,
        label: String,
        color: u8,
        icon: String,
        date: u64,
    ) -> Result<(), crate::error::FidoError>
    where
        P: MessageAllowed<EditSecurityKey>,
    {
        self.0.send_blocking_archive(EditSecurityKey { index, label, color, icon, date })
    }

    /// Set the archived state of a Security Key. Blocks until the server confirms; returns
    /// `FidoError::InvalidIndex` if the slot doesn't exist. Archived keys are automatically
    /// set to live=false.
    pub fn set_archived(&self, index: usize, archived: bool) -> Result<(), crate::error::FidoError>
    where
        P: MessageAllowed<SetArchived>,
    {
        self.0.try_send_blocking_scalar(SetArchived { index, archived })?
    }

    /// Synchronous snapshot of all security keys. Use at startup to populate local state
    /// before the async `SubscribeKeyChanges` stream has delivered its initial event.
    pub fn list_security_keys(&self) -> Vec<SecurityKeyView>
    where
        P: MessageAllowed<ListSecurityKeys>,
    {
        self.0.send_blocking_archive(ListSecurityKeys)
    }

    /// Get the index of the selected Security Key if any.
    pub fn selected_security_key_index(&self) -> Result<Option<usize>, crate::error::FidoError>
    where
        P: MessageAllowed<GetSelectedSecurityKey>,
    {
        Ok(self.0.try_send_blocking_scalar(GetSelectedSecurityKey)?)
    }

    /// Select/Deselect a Security Key for Registration (fire-and-forget).
    pub fn select_security_key(&self, index: Option<usize>)
    where
        P: MessageAllowed<SelectSecurityKey>,
    {
        self.0.try_send_scalar(SelectSecurityKey(index)).ok();
    }

    /* API for ctap-hid/nfc server */

    pub fn u2f_process_apdu(&self, msg: Vec<u8>, transport: Transport) -> Vec<u8>
    where
        P: MessageAllowed<U2fProcessApdu>,
    {
        self.0.send_blocking_archive(U2fProcessApdu { msg, transport })
    }

    pub fn ctap_process_cbor(&self, cmd: u8, raw: Vec<u8>) -> Vec<u8>
    where
        P: MessageAllowed<CtapProcessCbor>,
    {
        self.0.send_blocking_archive(CtapProcessCbor { cmd, raw })
    }

    /* API for Test Apps only */

    #[cfg(feature = "test-app")]
    pub fn reset_state(&mut self) -> Result<(), crate::error::FidoError>
    where
        P: MessageAllowed<ResetState>,
    {
        self.0.try_send_blocking_scalar(ResetState)?
    }
}
