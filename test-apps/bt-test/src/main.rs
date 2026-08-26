// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::thread;
use std::time::Duration;

use bt;
use bt::{messages::BlePacket, BluetoothError};
use log::{error, info};
use server::{xous::PID, ArchiveEventHandler, ServerContext};

bt::use_api!();

struct ListenerServer;

impl server::ServerMessages for ListenerServer {
    const NAME: &'static str = "";

    fn messages() -> &'static [server::MessageDef<Self>] { &[] }
}

impl server::Server for ListenerServer {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        info!("[*] Subscribing to BLE messages");
        let mut bt_api = BluetoothApi::default();
        bt_api.subscribe_ble(context);
    }
}

impl ArchiveEventHandler<BlePacket> for ListenerServer {
    fn handle(&mut self, packet: server::Owned<BlePacket>, _sender: PID, _context: &mut ServerContext<Self>) {
        info!("[+] BLE: {} bytes", packet.0.len());
    }
}

fn main() -> anyhow::Result<()> {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let mut bt_api = BluetoothApi::default();

    info!("[+] Waiting for boot");
    loop {
        let state = bt_api.state()?;
        info!("[+] State: {state:?}");
        if state.is_booted() {
            break;
        }
        std::thread::sleep(core::time::Duration::from_millis(1000));
    }

    info!("[*] Enabling BLE");
    bt_api.enable_ble()?;
    info!("[*] New state: {:?}", bt_api.state()?);

    info!("[*] Getting Bluetooth MAC address via public API (while enabled)");
    let bt_addr = bt_api.get_bt_addr()?;
    info!("[+] MAC: {}", hex::encode(bt_addr.as_slice()));

    info!("[*] Disabling BLE");
    bt_api.disable_ble()?;
    info!("[*] New state: {:?}", bt_api.state()?);

    info!("[*] Getting Bluetooth MAC address via public API (while disabled)");
    let bt_addr = bt_api.get_bt_addr()?;
    info!("[+] MAC: {}", hex::encode(bt_addr.as_slice()));

    info!("[*] Resetting the BLE controller");
    bt_api.reset()?;

    info!("[*] Enabling BLE while still booting");
    bt_api.enable_ble()?;

    info!("[+] Waiting for boot");
    loop {
        let state = bt_api.state()?;
        info!("[+] State: {state:?}");
        if state.is_booted() {
            break;
        }
        std::thread::sleep(core::time::Duration::from_millis(100));
    }

    std::thread::spawn(move || server::listen(ListenerServer));
    for msg_num in 1.. {
        match bt_api.send_ble(format!("BLE message #{msg_num}").as_bytes()) {
            Ok(_) => info!("[*] Sent BLE message #{}", msg_num),
            Err(BluetoothError::BlePacketRejected) => {
                error!("[!] BLE packet rejected, perhaps no device is connected?")
            }
            Err(e) => error!("[!] Error sending BLE message: {:?}", e),
        }
        let state = bt_api.state()?;
        info!("[+] State: {state:?}");
        thread::sleep(Duration::from_millis(1000));
    }

    Ok(())
}
