// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use core::fmt::Write;

use atsama5d27::uart::{Uart, Uart1};

pub type UartType = Uart<Uart1>;

fn main() -> ! {
    xous::set_thread_priority(xous::ThreadPriority::System7).unwrap();

    // Map the UART peripheral.
    let addr = xous::syscall::map_memory(
        xous::MemoryAddress::new(UartType::BASE_ADDRESS),
        None,
        0x1000,
        xous::MemoryFlags::W | xous::MemoryFlags::DEV,
    )
    .expect("couldn't map debug UART");

    let uart_addr = addr.as_mut_ptr() as _;
    let mut uart = UartType::with_alt_base_addr(uart_addr);
    let log_buffer =
        xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W).expect("Could not allocate buffer");
    writeln!(uart, "[LOG] Connecting to log server").ok();
    let log_reader = log_server::reader::LogReader::default();

    loop {
        let len = log_reader.read(log_buffer);
        for c in &log_buffer.as_slice()[..len] {
            uart.write_byte(*c)
        }
    }
}
