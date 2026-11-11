// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

// The upstream APPNAME, which hosts match on in the GET_APP_AND_VERSION reply.
app_flux_runtime::flux_app!("Tezos Wallet", entry = app_main);

app_flux_runtime::define_sdk_io_stubs!();
