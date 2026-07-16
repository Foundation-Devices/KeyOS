// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Write;

fn main() -> ! {
    let log_buffer =
        xous::map_memory(None, None, 0x4000, xous::MemoryFlags::W).expect("Could not allocate buffer");
    println!("[LOG] Connecting to log server");
    let log_reader = log_server::reader::LogReader::default();
    let mut stdout = std::io::stdout();

    loop {
        let len = log_reader.read(log_buffer);
        stdout.write_all(&log_buffer.as_slice()[..len]).unwrap();
    }
}
