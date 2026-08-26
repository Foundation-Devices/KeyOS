// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

nfc::use_api!();

pub fn main() -> () {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let mut nfc = NfcApi::default();
    let _ = nfc.set_enabled(true);
    let shard = backup_shard::Shard::default();
    let mut ndef_msg = ndef::Message::default();
    let mut ndef_rec1 = ndef::Record::new(None, ndef::Payload::from_cbor_encodable(&shard));
    ndef_msg.append_record(&mut ndef_rec1);
    match nfc.write_ndef_raw_msg(vec![], ndef_msg.to_vec(), Duration::from_millis(10000)) {
        Ok(()) => {
            log::info!("Wrote message");
        }
        Err(e) => {
            log::error!("Failed to write message: {:?}", e);
        }
    }
    match nfc.read_ndef_raw_msg(Duration::from_millis(10000)) {
        Ok((_, raw_msg)) => {
            log::info!("Read NDEF message ({} bytes)", raw_msg.len());
            match ndef::Message::try_from(raw_msg.as_slice()) {
                Ok(ndef_msg) => {
                    log::info!("Parsed {} NDEF record(s)", ndef_msg.records.len());
                }
                Err(e) => {
                    log::error!("Failed to parse NDEF message: {:?}", e);
                }
            }
        }
        Err(e) => {
            log::error!("Failed to read message: {:?}", e);
        }
    }
}
