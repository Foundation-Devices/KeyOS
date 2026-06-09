// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use app_flux_runtime::runtime::{self, RuntimeHooks};

app_flux_runtime::use_flux_runtime_api!();

extern "C" {
    pub fn app_main();
}

fn init_flux_runtime() {
    runtime::init(RuntimeHooks::new(
        flux_svc_call,
        flux_io_seph_send,
        flux_io_seph_recv,
        flux_syscall_buffer,
    ));
}

app_flux_runtime::define_sdk_io_stubs!();

#[no_mangle]
pub extern "C" fn os_pki_get_info(_key_id: u32, _info: *mut u8, _info_len: *mut u32) -> u32 {
    log::debug!("os_pki_get_info called - returning error");
    1
}

#[no_mangle]
pub extern "C" fn os_pki_verify(
    _key_id: u32,
    _hash: *const u8,
    _hash_len: u32,
    _signature: *const u8,
    _signature_len: u32,
) -> u32 {
    log::debug!("os_pki_verify called - returning error");
    1
}

app_flux_runtime::define_keyos_trace_stub!();

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("app-flux-monero starting");
    init_flux_runtime();

    unsafe {
        app_main();
    }
}
