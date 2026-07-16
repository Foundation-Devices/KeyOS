// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
mod atsama5d2;
#[cfg(keyos)]
use atsama5d2::PowerManagerServer;
#[cfg(keyos)]
mod atsama5d2_ext;
#[cfg(keyos)]
use atsama5d2_ext::PowerManagerServerExt;

#[cfg(not(keyos))]
mod hosted;
#[cfg(not(keyos))]
use hosted::{PowerManagerServer, PowerManagerServerExt};

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System7).unwrap();

    std::thread::spawn(|| server::listen(PowerManagerServer::new().unwrap()));
    server::listen(PowerManagerServerExt::new().unwrap())
}
