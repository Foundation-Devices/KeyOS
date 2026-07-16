// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(keyos))]
mod hosted;
#[cfg(not(keyos))]
use hosted::Server;

#[cfg(keyos)]
mod atsama5d2;
#[cfg(keyos)]
use atsama5d2::Server;

#[cfg(any(keyos, test))]
mod core;
#[cfg(any(keyos, test))]
mod downloader;
#[cfg(keyos)]
mod state;

#[derive(Debug, Copy, Clone, server::Message)]
struct DownloadStallTick;

crypto::use_api!();
fs::use_api!();
gui_server_api::use_api!();
power_manager::use_ext_api!();
quantum_link::use_api!();
security::use_api!();

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Debug);

    xous::set_thread_priority(xous::ThreadPriority::System3).unwrap();

    log::info!("Initializing update server");

    server::listen_with(Server::new)
}
