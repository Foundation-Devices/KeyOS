// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

app_flux_runtime::flux_app!("XRP", entry = app_main);

app_flux_runtime::define_sdk_io_stubs!();

// Upstream's library mode, which the exchange app runs for a swap, sets these; Legacy Mode never
// runs an app that way.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static mut G_called_from_swap: bool = false;
