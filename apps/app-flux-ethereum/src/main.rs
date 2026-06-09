// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::c_void;

use app_flux_runtime::runtime::{self, RuntimeHooks};

app_flux_runtime::use_flux_runtime_api!();

extern "C" {
    /// Entry point that initializes chainConfig and then calls app_main().
    /// Calling app_main() directly skips coin_main() initialization,
    /// leaving chainConfig as NULL and crashing on first APDU that uses it.
    pub fn eth_main();
}

fn init_flux_runtime() {
    runtime::init(RuntimeHooks::new(
        flux_svc_call,
        flux_io_seph_send,
        flux_io_seph_recv,
        flux_syscall_buffer,
    ));
}

// Swap stubs (HAVE_SWAP is enabled for type access but swap functionality is not used).
#[no_mangle]
pub extern "C" fn swap_copy_transaction_parameters(
    _params: *const c_void,
    _chain_config: *const c_void,
) -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn swap_handle_check_address(_params: *const c_void, _chain_config: *const c_void) {}

#[no_mangle]
pub extern "C" fn swap_handle_get_printable_amount(_params: *const c_void, _chain_config: *const c_void) {}

// Stubs for eth_plugin_handler.c (deleted on both ARM and hosted builds
// because it references ethPluginSharedRW_t which was removed from the plugin SDK).
#[no_mangle]
pub extern "C" fn eth_plugin_perform_init(
    _address: *const u8,
    _selector: *const u8,
    _init: *mut c_void,
) -> u32 {
    1
}

#[no_mangle]
pub extern "C" fn eth_plugin_prepare_init(_init: *mut c_void, _data: *const u8, _data_length: u32) {}

#[no_mangle]
pub extern "C" fn eth_plugin_prepare_provide_parameter(
    _param: *mut c_void,
    _data: *const u8,
    _data_length: u32,
) {
}

#[no_mangle]
pub extern "C" fn eth_plugin_call(_method: i32, _parameter: *mut c_void) -> u32 { 1 }

#[no_mangle]
pub extern "C" fn eth_plugin_prepare_finalize(_finalize: *mut c_void) {}

#[no_mangle]
pub extern "C" fn eth_plugin_prepare_provide_info(_info: *mut c_void) {}

#[no_mangle]
pub extern "C" fn eth_plugin_prepare_query_contract_ID(_query: *mut c_void) {}

#[no_mangle]
pub extern "C" fn eth_plugin_prepare_query_contract_UI(_query: *mut c_void) {}

// v1.21+ uses lowercase names for these functions.
#[no_mangle]
pub extern "C" fn eth_plugin_prepare_query_contract_id(_query: *mut c_void) {}

#[no_mangle]
pub extern "C" fn eth_plugin_prepare_query_contract_ui(_query: *mut c_void) {}

app_flux_runtime::define_pki_bypass_stubs!();

#[cfg(not(keyos))]
mod hosted_stubs {
    use core::ffi::c_void;

    #[no_mangle]
    pub static mut G_io_tx_buffer: [u8; 512] = [0; 512];

    #[no_mangle]
    pub extern "C" fn get_network_icon_from_chain_id(_chain_id: *const u64) -> *const c_void {
        core::ptr::null()
    }
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::debug!("app-flux-ethereum v3 starting (fixed touch_get_last_info layout)");
    init_flux_runtime();

    // Pre-initialize N_storage_real so that storage_init() sees initialized == true
    // and skips the bzero that would clear dataAllowed. This enables EIP-712 blind
    // signing which requires dataAllowed == true.
    //
    // internalStorage_t layout (with HAVE_TRANSACTION_CHECKS):
    //   offset 0: dataAllowed (bool)
    //   offset 8: initialized (bool)
    unsafe {
        extern "C" {
            static mut N_storage_real: [u8; 9];
        }
        N_storage_real[0] = 1;
        N_storage_real[8] = 1;
    }

    unsafe {
        eth_main();
    }
}
