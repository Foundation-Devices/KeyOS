// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use app_flux_runtime::runtime::{self, RuntimeHooks};

app_flux_runtime::use_flux_runtime_api!();

extern "C" {
    pub fn coin_main();
}

fn init_flux_runtime() {
    runtime::init(
        RuntimeHooks::new(flux_svc_call, flux_io_seph_send, flux_io_seph_recv, flux_syscall_buffer)
            .with_nvm_write(runtime::nvm_write_noop),
    );
}

app_flux_runtime::define_keyos_trace_stub!();

// Hosted-only stubs for C files skipped only during hosted compilation.
#[cfg(not(keyos))]
mod hosted_stubs {
    // --- Globals provided by SDK/app on ARM but not in hosted build ---

    #[no_mangle]
    pub static mut G_io_tx_buffer: [u8; 512] = [0; 512];

    /// G_io_app — global io_app_t (zero-initialized for hosted)
    #[no_mangle]
    pub static mut G_io_app: [u8; 256] = [0; 256];

    // --- Crypto (new hash init variants) ---

    #[no_mangle]
    pub extern "C" fn cx_ripemd160_init_no_throw(_ctx: *mut u8) -> u32 { 0 }

    #[no_mangle]
    pub extern "C" fn cx_blake2b_init2_no_throw(
        _ctx: *mut u8,
        _size: u32,
        _salt: *const u8,
        _salt_len: u32,
        _perso: *const u8,
        _perso_len: u32,
    ) -> u32 {
        0
    }

    // --- I/O subsystem stubs ---

    #[no_mangle]
    pub extern "C" fn io_seproxyhal_init() {}

    #[no_mangle]
    pub extern "C" fn USB_power(_enabled: u32) {}
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    log::info!("app-flux-zcash v2.5.0 starting");
    init_flux_runtime();

    unsafe {
        coin_main();
    }
    log::info!("coin_main() returned");
}
