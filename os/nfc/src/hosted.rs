// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use nfc::error::NfcError;

use crate::{NfcImpl, NfcServer};

pub struct Implementation;

impl Implementation {
    pub(crate) fn on_start_hook(&self, _context: &mut server::ServerContext<NfcServer>) {}
}

impl NfcImpl for Implementation {
    fn new() -> Implementation { Implementation }

    fn read_ndef_raw_msg(&mut self, _timeout: Duration) -> Result<(Vec<u8>, Vec<u8>), NfcError> {
        // TODO: Implement
        Ok((Vec::new(), Vec::new()))
    }

    fn write_ndef_raw_msg(
        &mut self,
        _uid: Vec<u8>,
        _msg: Vec<u8>,
        _timeout: Duration,
    ) -> Result<(), NfcError> {
        // TODO: Implement
        Ok(())
    }
}
