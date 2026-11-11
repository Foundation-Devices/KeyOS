// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared Flux SDK stub helpers.

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
