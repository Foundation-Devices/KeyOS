// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use bitflags::bitflags;
use server::{AsScalar, CheckedPermissions, FromScalar, MessageAllowed, Server, ServerContext};

pub mod error;
pub mod messages;

pub use error::BluetoothError;
use messages::*;

#[macro_export]
macro_rules! use_api {
    () => {
        mod bt_permissions {
            use bt::messages::*;
            #[derive(Clone, Default, server::Permissions)]
            #[server_name = "os/bt"]
            pub struct BluetoothPermissions;
        }
        type BluetoothApi = bt::BluetoothApi<bt_permissions::BluetoothPermissions>;
    };
}

#[derive(Default, Clone)]
pub struct BluetoothApi<P: CheckedPermissions> {
    conn: server::CheckedConn<P>,
}

impl<P: CheckedPermissions> BluetoothApi<P> {
    /// Gets the unique Bluetooth MAC address of the device.
    /// BLE controller must be in the main firmware state.
    ///
    /// # Errors
    ///
    /// - `BluetoothError::InvalidState` if the BLE controller is in bootloader state.
    pub fn get_bt_addr(&mut self) -> Result<BtAddr, BluetoothError>
    where
        P: MessageAllowed<GetBtAddr>,
    {
        self.conn.send_blocking_archive(GetBtAddr)
    }

    /// Enables the Bluetooth Low Energy (BLE) controller.
    ///
    /// This function will attempt to boot the BLE firmware if the controller is in bootloader state.
    pub fn enable_ble(&mut self) -> Result<(), BluetoothError>
    where
        P: MessageAllowed<EnableBle>,
    {
        self.conn.try_send_blocking_scalar(EnableBle)?
    }

    /// Disables the Bluetooth Low Energy (BLE) controller.
    ///
    /// This function will attempt to boot the BLE firmware if the controller is in bootloader state.
    pub fn disable_ble(&mut self) -> Result<(), BluetoothError>
    where
        P: MessageAllowed<DisableBle>,
    {
        self.conn.try_send_blocking_scalar(DisableBle)?
    }

    /// Disconnect from the Bluetooth Low Energy (BLE) central.
    pub fn disconnect(&mut self) -> Result<(), BluetoothError>
    where
        P: MessageAllowed<Disconnect>,
    {
        self.conn.try_send_blocking_scalar(Disconnect)?
    }

    /// Disables the given BLE advertising channels.
    ///
    /// This function will attempt to send a Channel Mask to the BLE stack in order to restrict channels
    /// usage.
    pub fn disable_adv_channels(&mut self, chans: AdvChannel) -> Result<(), BluetoothError>
    where
        P: MessageAllowed<DisableAdvChannels>,
    {
        self.conn.try_send_blocking_scalar(DisableAdvChannels(chans))?
    }

    /// Resets the BLE controller into the Bootloader state.
    pub fn reset(&mut self) -> Result<(), BluetoothError>
    where
        P: MessageAllowed<Reset>,
    {
        self.conn.try_send_blocking_scalar(Reset)?
    }

    /// Returns the current BLE controller's state.
    pub fn state(&mut self) -> Result<State, BluetoothError>
    where
        P: MessageAllowed<GetState>,
    {
        self.conn.try_send_blocking_scalar(GetState)?
    }

    /// Subscribes the server `S` to BLE packets.
    pub fn subscribe_ble<S>(&mut self, context: &mut ServerContext<S>)
    where
        S: Server + server::ArchiveEventHandler<BlePacket>,
        P: MessageAllowed<SubscribeBle>,
    {
        self.conn.subscribe_archive_infallible(SubscribeBle, context)
    }

    /// Subscribes the server `S` to BLE state changes.
    pub fn subscribe_ble_state<S>(&mut self, context: &mut ServerContext<S>)
    where
        S: Server + server::ScalarEventHandler<State>,
        P: MessageAllowed<SubscribeBleState>,
    {
        self.conn.subscribe_scalar_infallible(SubscribeBleState, context)
    }

    /// Sends a BLE packet.
    pub fn send_ble(&mut self, data: &[u8]) -> Result<(), BluetoothError>
    where
        P: MessageAllowed<SendBle>,
    {
        self.conn.send_blocking_archive(SendBle(data.to_vec()))
    }

    pub fn test_echo(&mut self, size: usize, character: u8) -> Result<(), BluetoothError>
    where
        P: MessageAllowed<TestEcho>,
    {
        self.conn.send_blocking_archive(TestEcho { size, character })
    }

    /// Returns the version of the BLE controller's bootloader and firmware.
    pub fn get_version_info(&mut self) -> Option<BleVersionInfo>
    where
        P: MessageAllowed<GetBleVersionInfo>,
    {
        self.conn.send_blocking_archive(GetBleVersionInfo)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[repr(u8)]
pub enum State {
    Booting,
    StartingFirmware,
    Disabled,
    WaitingForConnection,
    Connected { rssi: i8 },
    Unknown,
}

impl State {
    pub fn is_booted(&self) -> bool {
        matches!(self, State::Disabled)
            || matches!(self, State::WaitingForConnection)
            || matches!(self, State::Connected { .. })
    }

    pub fn is_connected(&self) -> bool { matches!(self, State::Connected { .. }) }

    pub fn is_enabled(&self) -> bool {
        matches!(self, State::WaitingForConnection) || matches!(self, State::Connected { .. })
    }
}

impl AsScalar<2> for State {
    fn as_scalar(&self) -> [u32; 2] {
        match self {
            State::Booting => [0, 0],
            State::StartingFirmware => [1, 0],
            State::Disabled => [2, 0],
            State::WaitingForConnection => [3, 0],
            State::Connected { rssi } => [4, *rssi as u32],
            State::Unknown => [5, 0],
        }
    }
}

impl FromScalar<2> for State {
    fn from_scalar(value: [u32; 2]) -> Self {
        match value[0] {
            0 => State::Booting,
            1 => State::StartingFirmware,
            2 => State::Disabled,
            3 => State::WaitingForConnection,
            4 => State::Connected { rssi: value[1] as i8 },
            _ => State::Unknown,
        }
    }
}

bitflags! {
    #[derive(Debug, Clone)]
    pub struct AdvChannel: u8 {
        const C39 = 1 << 7;
        const C38 = 1 << 6;
        const C37 = 1 << 5;
    }
}

impl AsScalar<1> for AdvChannel {
    fn as_scalar(&self) -> [u32; 1] { [self.bits() as u32] }
}

impl FromScalar<1> for AdvChannel {
    fn from_scalar([value]: [u32; 1]) -> Self { AdvChannel::from_bits_truncate(value as u8) }
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct BleVersionInfo {
    pub bootloader_version: String,
    pub firmware_version: Option<String>,
}

#[derive(Debug, Copy, Clone, rkyv::Serialize, rkyv::Deserialize, rkyv::Archive)]
pub struct BtAddr([u8; 6]);

impl BtAddr {
    pub const fn new(addr: [u8; 6]) -> Self { Self(addr) }

    pub fn as_slice(&self) -> &[u8] { self.0.as_slice() }

    pub fn to_hex_string(&self) -> String {
        self.0.iter().map(|byte| format!("{:02X}", byte)).collect::<Vec<String>>().join(":")
    }
}

impl From<[u8; 6]> for BtAddr {
    fn from(value: [u8; 6]) -> Self { BtAddr(value) }
}
