// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared Flux SDK stub helpers.

pub fn keyos_trace(id: u32) {
    log::trace!("TRACE({})", id);
    server::xous::yield_slice();
    server::xous::yield_slice();
}

#[macro_export]
macro_rules! define_sdk_io_stubs {
    () => {
        // SDK I/O global buffers and UX helper are app-provided for some SDK builds.
        #[allow(non_upper_case_globals)]
        #[no_mangle]
        pub static mut G_io_tx_buffer: [u8; 512] = [0; 512];

        #[allow(non_upper_case_globals)]
        #[no_mangle]
        pub static mut G_io_rx_buffer: [u8; 512] = [0; 512];

        #[no_mangle]
        pub extern "C" fn os_io_handle_ux_event_reject_apdu() -> u32 { 0 }
    };
}

#[macro_export]
macro_rules! define_keyos_trace_stub {
    () => {
        // Debug trace function callable from C code.
        // We yield after each log to ensure the IPC message is delivered
        // before the process can crash, making traces visible even on fast crashes.
        #[no_mangle]
        pub extern "C" fn keyos_trace(id: u32) { $crate::stubs::keyos_trace(id) }
    };
}
