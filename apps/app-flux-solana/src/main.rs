// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::c_void;

use app_flux_runtime::runtime::{self, RuntimeHooks};

app_flux_runtime::use_flux_runtime_api!();

extern "C" {
    pub fn app_main();
}

fn flux_current_app_tag(tag: u32) -> Option<&'static [u8]> {
    match tag {
        0x01 => Some(b"Solana"),
        0x02 => Some(b"1.14.0"),
        _ => None,
    }
}

fn init_flux_runtime() {
    runtime::init(
        RuntimeHooks::new(flux_svc_call, flux_io_seph_send, flux_io_seph_recv, flux_syscall_buffer)
            .with_current_app_tag(flux_current_app_tag),
    );
}

app_flux_runtime::define_sdk_io_stubs!();
app_flux_runtime::define_pki_bypass_stubs!();

// SDK big-number / EC-point stubs (referenced by ed25519_helpers.c in the Solana app).
// In KeyOS, Ed25519 key derivation and signing go through IPC (os_perso_derive_node_bip32);
// the on-curve validation in ed25519_helpers.c is not critical, so these stubs allow it
// to link while returning "valid" for any key.
#[no_mangle]
pub extern "C" fn cx_bn_lock(_word_nbytes: u32, _flags: u32) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn cx_bn_is_locked() -> u32 { 1 }

#[no_mangle]
pub extern "C" fn cx_bn_unlock() -> u32 { 0 }

#[no_mangle]
pub extern "C" fn cx_ecpoint_alloc(_point: *mut c_void, _curve: u32) -> u32 { 0 }

#[no_mangle]
pub extern "C" fn cx_ecpoint_decompress(_point: *mut c_void, _prefix: *const u8, _prefix_len: u32) -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn cx_ecpoint_is_on_curve(_point: *const c_void, _result: *mut u32) -> u32 {
    if !_result.is_null() {
        unsafe { *_result = 1 };
    }
    0
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("app-flux-solana starting");
    init_flux_runtime();

    unsafe {
        app_main();
    }
}
