// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod api;
#[cfg(keyos)]
mod hid;
mod implementation;
pub mod messages;

pub fn listen() { server::listen(implementation::LegacyHidServer::default()) }
