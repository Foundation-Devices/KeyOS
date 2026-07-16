// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use bt::{messages::*, BluetoothError, State};
use server::{
    ArchiveEventSubscriber, ArchiveEventSubscriptionHandler, BlockingArchiveHandler, BlockingScalarHandler,
    MessageId as _, ScalarEventSubscriber, ScalarEventSubscriptionHandler, ScalarHandler, Server,
};

#[cfg(not(keyos))]
mod hosted;
#[cfg(not(keyos))]
use hosted::BluetoothServer;
#[cfg(keyos)]
mod atsama5d2;
#[cfg(keyos)]
use atsama5d2::BluetoothServer;

settings::use_api!();

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System6).unwrap();

    log::info!("Initializing bt");

    server::listen(BluetoothServer::default())
}

impl Server for BluetoothServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        self.on_start_hook(context);
        xous::register_system_event_handler(
            xous::SystemEvent::Disconnected,
            context.sid(),
            SubscriberDisconnected::ID,
        )
        .unwrap();

        SettingsApi::default().server_subscribe_bluetooth_enabled(context);
    }
}

impl server::ScalarEventHandler<settings::global::BluetoothEnabled> for BluetoothServer {
    fn handle(
        &mut self,
        msg: settings::global::BluetoothEnabled,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        if msg.0 {
            self.enable().inspect_err(|e| log::warn!("failed to enable bt {e:?}")).ok();
        } else {
            self.disable().inspect_err(|e| log::warn!("failed to disable bt {e:?}")).ok();
        }
    }
}

impl BlockingArchiveHandler<GetBtAddr> for BluetoothServer {
    fn handle(
        &mut self,
        _msg: GetBtAddr,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetBtAddr as server::BlockingArchive>::Response {
        self.get_bt_addr()
    }
}

impl BlockingScalarHandler<EnableBle> for BluetoothServer {
    fn handle(
        &mut self,
        _msg: EnableBle,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), BluetoothError> {
        self.enable()
    }
}

impl BlockingScalarHandler<DisableBle> for BluetoothServer {
    fn handle(
        &mut self,
        _msg: DisableBle,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), BluetoothError> {
        self.disable()
    }
}

impl BlockingScalarHandler<Reset> for BluetoothServer {
    fn handle(
        &mut self,
        _msg: Reset,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), BluetoothError> {
        self.reset();
        Ok(())
    }
}

impl BlockingScalarHandler<Disconnect> for BluetoothServer {
    fn handle(
        &mut self,
        _msg: Disconnect,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), BluetoothError> {
        self.disconnect()
    }
}

impl BlockingScalarHandler<GetState> for BluetoothServer {
    fn handle(
        &mut self,
        _msg: GetState,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<State, BluetoothError> {
        if self.state.is_enabled() {
            self.refresh_connection()?;
        }
        Ok(self.state)
    }
}

impl ArchiveEventSubscriptionHandler<SubscribeBle> for BluetoothServer {
    fn handle(
        &mut self,
        _msg: SubscribeBle,
        subscriber: ArchiveEventSubscriber<BlePacket>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.packet_subscribers.push(subscriber);
        Ok(())
    }
}

impl ScalarEventSubscriptionHandler<SubscribeBleState> for BluetoothServer {
    fn handle(
        &mut self,
        _msg: SubscribeBleState,
        subscriber: ScalarEventSubscriber<State>,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        if subscriber.send(&self.state).is_ok() {
            self.state_subscribers.push(subscriber);
        }
        Ok(())
    }
}

impl BlockingScalarHandler<DisableAdvChannels> for BluetoothServer {
    fn handle(
        &mut self,
        msg: DisableAdvChannels,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), BluetoothError> {
        self.disable_adv_channels(msg.0)
    }
}

impl BlockingArchiveHandler<SendBle> for BluetoothServer {
    fn handle(
        &mut self,
        msg: SendBle,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), BluetoothError> {
        self.send(&msg.0)
    }
}

impl ScalarHandler<Poll> for BluetoothServer {
    fn handle(&mut self, _msg: Poll, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        self.poll();
    }
}

impl ScalarHandler<SubscriberDisconnected> for BluetoothServer {
    fn handle(
        &mut self,
        msg: SubscriberDisconnected,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) {
        self.packet_subscribers.remove_cid(msg.0);
        self.state_subscribers.retain(|s| s.cid() != msg.0);
    }
}

impl BlockingArchiveHandler<TestEcho> for BluetoothServer {
    fn handle(
        &mut self,
        msg: TestEcho,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> Result<(), BluetoothError> {
        self.test_echo(msg.size, msg.character)
    }
}

impl BlockingArchiveHandler<GetBleVersionInfo> for BluetoothServer {
    fn handle(
        &mut self,
        _msg: GetBleVersionInfo,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> <GetBleVersionInfo as server::BlockingArchive>::Response {
        self.get_version_info()
    }
}

#[derive(Debug, server::Message)]
pub struct SubscriberDisconnected(pub xous::CID);
