// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared Flux SDK stub helpers.

pub fn pki_get_info_bypass(key_usage: *mut u8) -> u32 {
    log::debug!("os_pki_get_info: bypassing PKI (not available on KeyOS)");
    if !key_usage.is_null() {
        unsafe {
            *key_usage = 0;
        }
    }
    0
}

pub fn pki_verify_bypass() -> u32 {
    log::debug!("os_pki_verify: bypassing PKI (not available on KeyOS)");
    1
}

pub fn keyos_trace(id: u32) {
    log::trace!("TRACE({})", id);
    server::xous::yield_slice();
    server::xous::yield_slice();
}

#[macro_export]
macro_rules! define_pki_bypass_stubs {
    () => {
        /// Retrieve PKI certificate info. On KeyOS we don't have access to the
        /// certificate chain, so we return success with a neutral key_usage.
        #[no_mangle]
        pub extern "C" fn os_pki_get_info(
            key_usage: *mut u8,
            _trusted_name: *mut u8,
            _trusted_name_len: *mut u32,
            _public_key: *mut u8,
        ) -> u32 {
            $crate::stubs::pki_get_info_bypass(key_usage)
        }

        /// Verify a signature against a previously loaded PKI certificate.
        /// On KeyOS, Flux apps are already verified at install time, so we
        /// return true (valid) to let the calling code proceed normally.
        #[no_mangle]
        pub extern "C" fn os_pki_verify(
            _hash: *const u8,
            _hash_len: u32,
            _sig: *const u8,
            _sig_len: u32,
        ) -> u32 {
            $crate::stubs::pki_verify_bypass()
        }
    };
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
