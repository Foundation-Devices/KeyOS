// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Append battery and screen state to a CSV every 30 seconds.
//!
//! Not part of any firmware, dev builds included. To use it, add "power-log" to
//! `DEFAULT_SERVICES_NORMAL` in `xtask/src/main.rs` and rebuild. Passing `--services`
//! instead replaces the whole list rather than adding to it.
//!
//! The log lands on the encrypted volume, which the file browser calls "Internal".
//! `plot-power-log.py` next to this file charts it.

use std::{
    io::{Seek, SeekFrom, Write},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs::{Location, OpenFlags};
use power_manager::ChargeStatus;

fs::use_api!();
gui_server_api::use_api!();
power_manager::use_ext_api!();

const SAMPLE_INTERVAL: Duration = Duration::from_secs(30);
const LOG_PATH: &str = "power-log.csv";
const CSV_HEADER: &str = "timestamp,battery_percent,voltage_mv,current_ma,remaining_capacity_mah,\
                          full_capacity_mah,charging,lcd_on\n";

fn main() -> ! {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let fs = FileSystem::default();
    // The encrypted volume only exists from the first unlock, so there is nothing to open before it.
    fs.wait_for_filesystem(Location::User);

    let mut file = fs
        .open_file(LOG_PATH, Location::User, OpenFlags { read: false, write: true, create: true })
        .expect("Could not open the power log");
    if file.seek(SeekFrom::End(0)).expect("Could not seek the power log") == 0 {
        file.write_all(CSV_HEADER.as_bytes()).expect("Could not write the power log header");
    }

    let power_manager = PowerManagerExtApi::default();
    let gui = GuiApiLight::connect();

    loop {
        let status = power_manager.status().expect("Could not read the charge status");
        // The hosted power manager has no fuel gauge and always answers None, so this is hardware only.
        let battery = power_manager.extended_status().expect("Could not read the fuel gauge");
        let charging = matches!(status.charge_status, ChargeStatus::Charging | ChargeStatus::ChargeDone);
        // Locking is what turns the panel off, so the lock state stands in for the LCD state.
        let lcd_on = !gui.is_locked().unwrap();

        let record = format!(
            "{},{},{},{},{},{},{},{}\n",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            status.battery_percent,
            battery.voltage_mv,
            battery.current,
            battery.remaining_capacity_mah,
            battery.capacity_mah,
            charging as u8,
            lcd_on as u8,
        );
        // Flush every record, or a flat battery takes the interesting end of the log with it.
        if let Err(e) = file.write_all(record.as_bytes()).and_then(|_| file.flush()) {
            log::warn!("Could not write the power log: {e}");
        }

        std::thread::sleep(SAMPLE_INTERVAL);
    }
}
