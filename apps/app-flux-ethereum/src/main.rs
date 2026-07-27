// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::c_void;

// `eth_main` (not `app_main`) is the C entry: it runs coin_main() to initialize chainConfig first;
// calling app_main() directly leaves chainConfig NULL and crashes on the first APDU that uses it.
app_flux_runtime::flux_app!("Ethereum", entry = eth_main);

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
