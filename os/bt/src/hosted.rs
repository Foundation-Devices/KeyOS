// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use bt::{messages::*, AdvChannel, BleVersionInfo, BluetoothError, BtAddr, State};
use server::{ArchiveSubList, ScalarEventSubscriber, ServerContext};

use crate::SubscriberDisconnected;

#[derive(server::Server)]
#[name = "os/bt"]
pub struct BluetoothServer {
    pub packet_subscribers: ArchiveSubList<BlePacket>,
    pub state_subscribers: Vec<ScalarEventSubscriber<State>>,
    pub state: State,
}

impl Default for BluetoothServer {
    fn default() -> Self {
        Self { packet_subscribers: Default::default(), state_subscribers: Vec::new(), state: State::Booting }
    }
}

impl BluetoothServer {
    pub fn refresh_connection(&mut self) -> Result<(), BluetoothError> { Ok(()) }

    pub(crate) fn set_state(&mut self, new_state: State) {
        if self.state != new_state {
            self.state = new_state;
            self.state_subscribers.retain(|s| s.send(&new_state).is_ok());
        }
    }

    pub fn on_start_hook(&mut self, _context: &mut ServerContext<Self>) {}

    pub fn enable(&mut self) -> Result<(), BluetoothError> {
        log::debug!("Enable called in hosted mode");
        self.set_state(State::WaitingForConnection);
        Ok(())
    }

    pub fn disable(&mut self) -> Result<(), BluetoothError> {
        log::debug!("Disable called in hosted mode");
        self.set_state(State::Disabled);
        Ok(())
    }

    pub fn disable_adv_channels(&mut self, chans: AdvChannel) -> Result<(), BluetoothError> {
        log::debug!("DisableAdvChannels({chans:?}) called in hosted mode");
        Ok(())
    }

    pub fn get_bt_addr(&mut self) -> Result<BtAddr, BluetoothError> {
        if !self.state.is_booted() {
            return Err(BluetoothError::InvalidState);
        }
        Ok(BtAddr::new([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]))
    }

    pub fn reset(&mut self) {
        log::debug!("Reset called in hosted mode");
        self.set_state(State::Disabled);
    }

    pub fn disconnect(&mut self) -> Result<(), BluetoothError> {
        log::debug!("Disconnect called in hosted mode");
        if !self.state.is_connected() {
            return Err(BluetoothError::InvalidState);
        }
        self.set_state(State::WaitingForConnection);
        Ok(())
    }

    pub fn send(&mut self, data: &[u8]) -> Result<(), BluetoothError> {
        if !self.state.is_enabled() {
            return Err(BluetoothError::InvalidState);
        }
        log::trace!("Send({data:02x?}) called in hosted mode");
        Ok(())
    }

    pub fn poll(&mut self) {}

    pub fn test_echo(&mut self, _size: usize, _character: u8) -> Result<(), BluetoothError> { Ok(()) }

    pub fn get_version_info(&mut self) -> Option<BleVersionInfo> { None }
}
