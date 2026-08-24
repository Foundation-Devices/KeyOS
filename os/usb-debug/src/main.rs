// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Debug channel for passport-drive: screenshots, input injection, app uploads and log streaming.
//!
//! The wire format lives in `usb-debug-protocol`, which is the source of truth for command bytes
//! and payload encoding. Commands are dispatched by [`dispatch`] whichever way they arrive: over
//! the vendor-specific USB interface on hardware ([`usb`]), over a loopback socket on the simulator
//! ([`sim`]).

mod dispatch;
#[cfg(keyos)]
mod msos20;
#[cfg(not(keyos))]
mod sim;
#[cfg(keyos)]
mod usb;

/// Max pending log messages in the channel before the log drain starts dropping.
/// Each chunk is up to 16 KB, so 8 chunks ≈ 128 KB max buffered.
const MAX_PENDING_LOGS: usize = 8;

fn main() -> ! {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::AppBackground0).unwrap();

    #[cfg(keyos)]
    usb::run();
    #[cfg(not(keyos))]
    sim::run()
}
