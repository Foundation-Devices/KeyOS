// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(not(keyos))]
mod hosted;
#[cfg(not(keyos))]
pub use hosted::*;

#[cfg(keyos)]
mod atsama5d2;
#[cfg(keyos)]
pub use atsama5d2::*;

settings::use_api!();

impl server::Server for CameraServer {
    fn on_start(&mut self, context: &mut server::ServerContext<Self>) {
        self.start(context);
        SettingsApi::default().server_subscribe_camera_enabled(context);
    }
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System0).unwrap();

    server::listen(CameraServer::default());
}
