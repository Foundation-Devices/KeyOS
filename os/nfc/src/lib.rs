// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
mod atsama5d2;

use std::time::{Duration, Instant};

#[cfg(keyos)]
use atsama5d2::Implementation;

#[cfg(not(keyos))]
mod hosted;
#[cfg(not(keyos))]
use hosted::Implementation;
use nfc::{error::NfcError, messages::*};
use server::{BlockingArchiveHandler, BlockingScalar, BlockingScalarHandler, Server, ServerContext};

settings::use_api!();

/// If the time since the last access is less than this, we report being active.
const ACTIVE_STATUS_THRESHOLD: Duration = Duration::from_millis(1500);

trait NfcImpl {
    fn new() -> Result<Implementation, NfcError>;
    fn read_ndef_raw_msg(&mut self, timeout: Duration) -> Result<(Vec<u8>, Vec<u8>), NfcError>;
    fn write_ndef_raw_msg(&mut self, uid: Vec<u8>, msg: Vec<u8>, timeout: Duration) -> Result<(), NfcError>;
}

#[derive(server::Server)]
#[name = "os/nfc"]
struct NfcServer {
    implementation: Implementation,
    enabled: bool,
    last_access: Option<Instant>,
}

impl NfcServer {
    pub fn new() -> Result<Self, NfcError> {
        let implementation = Implementation::new()?;
        Ok(Self { implementation, enabled: false, last_access: None })
    }
}

impl Server for NfcServer {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        self.implementation.on_start_hook(context);
        SettingsApi::default().server_subscribe_nfc_enabled(context);
    }
}

impl server::ScalarEventHandler<settings::global::NfcEnabled> for NfcServer {
    fn handle(
        &mut self,
        msg: settings::global::NfcEnabled,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) {
        self.enabled = msg.0;
    }
}

#[cfg(keyos)]
impl server::ScalarEventHandler<gpio::IrqMessage> for NfcServer {
    fn handle(
        &mut self,
        _msg: gpio::IrqMessage,
        _pid: std::num::NonZeroU8,
        _context: &mut ServerContext<Self>,
    ) {
        log::trace!("Got GPIO low");
        #[cfg(keyos)]
        self.implementation.irq_out_handler(self.enabled);
    }
}

impl BlockingArchiveHandler<ReadNdefRawMsg> for NfcServer {
    fn handle(
        &mut self,
        ReadNdefRawMsg(timeout): ReadNdefRawMsg,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(Vec<u8>, Vec<u8>), NfcError> {
        if !self.enabled {
            return Err(NfcError::Disabled);
        }
        let result = self.implementation.read_ndef_raw_msg(timeout);
        self.last_access = Some(Instant::now());
        result
    }
}

impl BlockingArchiveHandler<WriteNdefRawMsg> for NfcServer {
    fn handle(
        &mut self,
        WriteNdefRawMsg((uid, msg, timeout)): WriteNdefRawMsg,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), NfcError> {
        if !self.enabled {
            return Err(NfcError::Disabled);
        }
        let result = self.implementation.write_ndef_raw_msg(uid, msg, timeout);
        self.last_access = Some(Instant::now());
        result
    }
}

impl BlockingScalarHandler<SetEnabled> for NfcServer {
    fn handle(
        &mut self,
        msg: SetEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <SetEnabled as BlockingScalar>::Response {
        self.enabled = msg.0;
    }
}

impl BlockingScalarHandler<IsEnabled> for NfcServer {
    fn handle(
        &mut self,
        _msg: IsEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <IsEnabled as server::BlockingScalar>::Response {
        self.enabled
    }
}

impl BlockingScalarHandler<IsActive> for NfcServer {
    fn handle(
        &mut self,
        _msg: IsActive,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <IsActive as server::BlockingScalar>::Response {
        self.last_access.map(|la| la.elapsed() <= ACTIVE_STATUS_THRESHOLD).unwrap_or(false)
    }
}

pub fn listen() { server::listen(NfcServer::new().unwrap()) }
