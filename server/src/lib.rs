// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! A type-safe way to implement KeyOS servers.

mod archive;
mod blocking_archive;
mod buffer;
mod checked_conn;
mod deferred;
mod definitions;
mod error;
mod event;
mod lend_mut;
mod macros;
mod r#move;
mod owned;
mod rkyv_utils;
mod scalar;
mod server;

pub use archive::*;
pub use blocking_archive::*;
pub use buffer::*;
pub use checked_conn::*;
pub use deferred::*;
pub use definitions::*;
pub use error::*;
pub use event::*;
pub use lend_mut::*;
pub use owned::*;
pub use r#move::*;
pub use rkyv_utils::*;
pub use scalar::*;
pub use server::*;
pub use server_macro::*;
// Re-export related modules so other crates don't need to depend on them directly.
pub use {xous, xous_names};

pub(crate) fn next_dynamic_message_id() -> xous::MessageId {
    use std::sync::atomic::AtomicUsize;
    static DYNAMIC_MESSAGE_ID: AtomicUsize = AtomicUsize::new(0x10000);

    DYNAMIC_MESSAGE_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}
