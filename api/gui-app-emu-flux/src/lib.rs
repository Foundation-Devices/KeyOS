// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod api;
pub mod keys;
pub mod messages;
pub mod syscall_id;

pub use api::{FluxApi, SERVER_NAME};

/// Upstream's `os_perso_get_master_key_identifier` (its upstream id), newer than the API level
/// [`syscall_id`] is generated from. The emulator writes the 20-byte identifier into the buffer.
pub const SYSCALL_OS_PERSO_GET_MASTER_KEY_IDENTIFIER_ID: u32 = 0x0200_0055;
