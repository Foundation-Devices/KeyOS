// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use power_manager::messages::*;
use power_manager::{AttachedState, ChargeStatus, PowerManagerError, Status};
use server::{
    BlockingArchiveHandler, BlockingScalar, BlockingScalarHandler, ScalarEventSubscriber,
    ScalarEventSubscriptionHandler, ScalarHandler, ServerContext,
};
use xous::PID;

#[derive(server::Server)]
#[name = "os/power-manager"]
pub struct PowerManagerServer;
impl server::Server for PowerManagerServer {}

#[derive(server::Server)]
#[name = "os/power-manager-ext"]
pub struct PowerManagerServerExt {
    boosting: bool,
    battery_percent: u8,
    status_update_subscribers: Vec<ScalarEventSubscriber<Status>>,
    last_status: Option<Status>,
}
impl server::Server for PowerManagerServerExt {}

#[derive(Debug, server::Message)]
pub struct SetBatteryPercent(pub(crate) u8);

impl PowerManagerServer {
    pub fn new() -> Result<Self, PowerManagerError> { Ok(Self) }
}

impl PowerManagerServerExt {
    pub fn new() -> Result<Self, PowerManagerError> {
        Ok(Self {
            boosting: false,
            battery_percent: 80,
            last_status: None,
            status_update_subscribers: Vec::new(),
        })
    }
}

impl BlockingScalarHandler<Reboot> for PowerManagerServer {
    fn handle(&mut self, _msg: Reboot, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        log::info!("[!] Reboot requested, shutting down instead");
        xous::rsyscall(xous::SysCall::Shutdown(0)).unwrap();
    }
}

impl BlockingScalarHandler<Shutdown> for PowerManagerServerExt {
    fn handle(&mut self, _msg: Shutdown, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        log::info!("[!] Shutting down");
        xous::rsyscall(xous::SysCall::Shutdown(0)).unwrap();
    }
}

impl BlockingScalarHandler<GetStatus> for PowerManagerServerExt {
    fn handle(
        &mut self,
        _msg: GetStatus,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetStatus as BlockingScalar>::Response {
        Status {
            charge_status: ChargeStatus::Charging,
            battery_percent: self.battery_percent,
            attached_state: AttachedState::Source,
        }
    }
}

impl BlockingScalarHandler<SetUsbBoost> for PowerManagerServerExt {
    fn handle(
        &mut self,
        msg: SetUsbBoost,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> SetUsbBoostResponse {
        log::info!("SetUsbBoost called with enabled={}", msg.enabled);
        let previous = self.boosting;
        self.boosting = msg.enabled;
        SetUsbBoostResponse { success: true, previous_state: previous }
    }
}

impl ScalarHandler<SetBatteryPercent> for PowerManagerServerExt {
    fn handle(
        &mut self,
        msg: SetBatteryPercent,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.battery_percent = msg.0;
        self.last_status = Some(Status {
            charge_status: ChargeStatus::Idle,
            battery_percent: msg.0,
            attached_state: AttachedState::Source,
        })
    }
}

impl BlockingArchiveHandler<GetExtendedStatus> for PowerManagerServerExt {
    fn handle(
        &mut self,
        _msg: GetExtendedStatus,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <GetExtendedStatus as server::BlockingArchive>::Response {
        None
    }
}

impl ScalarHandler<ClearChargeFault> for PowerManagerServerExt {
    fn handle(
        &mut self,
        _msg: ClearChargeFault,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::info!("ClearChargeFault called, but not implemented in hosted mode");
    }
}

impl ScalarEventSubscriptionHandler<StatusSubscribe> for PowerManagerServerExt {
    fn handle(
        &mut self,
        _msg: StatusSubscribe,
        subscriber: ScalarEventSubscriber<Status>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        let status = Status {
            charge_status: ChargeStatus::Idle,
            battery_percent: self.battery_percent,
            attached_state: AttachedState::Source,
        };

        if subscriber.send(&status).is_err() {
            // If we couldn't send the update, then we won't add a subscriber
            return Ok(());
        }

        self.status_update_subscribers.push(subscriber);

        Ok(())
    }
}
