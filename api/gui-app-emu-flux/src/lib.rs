// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod api;
pub mod keys;
pub mod messages;
pub mod syscall_id;

pub use api::{FluxApi, SERVER_NAME};

/// Not an upstream syscall, so it sits outside the generated id space in [`syscall_id`]: the
/// channel a child uses to declare the BIP32 roots its upstream app allows, which the emulator
/// then holds it to. Buffer layout is one `[len][element...]` group per root, big-endian u32,
/// with the root count in `arg`.
pub const SYSCALL_DECLARE_DERIVATION_ROOTS_ID: u32 = 0xffff_0001;
