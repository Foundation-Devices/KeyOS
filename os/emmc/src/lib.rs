// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg(any(keyos, test))]
pub const BLOCK_SIZE: usize = 512;

#[cfg(keyos)]
pub mod api;
#[cfg(keyos)]
mod atsama5d2;
pub mod error;
#[cfg(keyos)]
mod handlers;
pub mod messages;
mod pipeline;

#[cfg(keyos)]
crypto::use_api!();

pub(crate) const SD_BUFFER_BLOCKS: usize = 64;
const _: () = {
    if (SD_BUFFER_BLOCKS * BLOCK_SIZE) % 4096 != 0 {
        panic!("SD buffer size must be divisible by page size")
    }
};

#[cfg(keyos)]
pub fn listen() { server::listen(atsama5d2::EmmcServer::new().unwrap()) }
