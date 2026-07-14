// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
pub mod device;
#[cfg(keyos)]
pub mod host;
#[cfg(keyos)]
pub mod subscription;

#[cfg(keyos)]
use std::sync::{LazyLock, Mutex};

#[cfg(keyos)]
pub static DEVICE_NAME: LazyLock<Mutex<String>> = LazyLock::new(|| {
    #[cfg(feature = "recovery-os")]
    {
        Mutex::new(String::from("recovery"))
    }
    #[cfg(not(feature = "recovery-os"))]
    {
        Mutex::new(String::from("unnamed"))
    }
});
power_manager::use_api!();
power_manager::use_ext_api!();

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    xous::set_thread_priority(xous::ThreadPriority::System5).unwrap();

    #[cfg(keyos)]
    {
        std::thread::spawn(|| server::listen(device::implementation::UsbDeviceServer::new()));

        std::thread::spawn(|| server::listen(subscription::SubscriptionServer::default()));

        server::listen(host::implementation::UsbHostServer::new())
    }
}
