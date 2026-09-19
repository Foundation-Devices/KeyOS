#![no_std]
#![warn(missing_docs)]
//! This crate contains a simplified driver for the Enhanced Host Controller Interface
//! (the USB 2.0 standard host interface)
//!
//! Currently only a tiny subset of functionality is implemented that's necessary
//! to implement the Bulk-Only Mass Storage protocol.

mod controller;
pub mod descriptors;
mod error;
mod pool;
mod queue;
mod registers;
mod transfer;
mod util;

pub use controller::{max_enumeration_ms, Controller, EventHandler, MAX_CONTROL_DATA_LEN};
pub use error::EhciError;
pub use transfer::ControlRequest;
use pool::{Pool, PoolElement};
pub use queue::EndpointDirection;
use registers::{Qtd, QueueHead};
pub use transfer::TransferResult;

/// Element type for the Queue Head Pool that has to be used to initialize the controller.
pub type QueueHeadPoolElement = PoolElement<QueueHead>;
/// Queue Head pool used to initialize the controller.
pub type QueueHeadPool = Pool<QueueHead>;

/// Element type for the Qtd Pool that has to be used to initialize the controller.
pub type QtdPoolElement = PoolElement<Qtd>;
/// Qtd pool used to initialize the controller.
pub type QtdPool = Pool<Qtd>;

/// Element type for the temporary buffer pool used to initialize the controller.
pub type BufferPoolElement = PoolElement<[u8; 0x40]>;
/// Temporary buffer pool used to initialize the controller.
pub type BufferPool = Pool<[u8; 0x40]>;

/// A context that can return the buffer it contains.
pub trait TransferContext {
    /// The buffer the context contains. Used for data reads and writes.
    /// Should stay valid and in place for the lifetime of the context.
    fn data_buffer(&mut self) -> &mut [u8];

    /// Setup data buffer the context contains, if applicable.
    /// Should stay valid and in place for the lifetime of the context.
    fn setup_buffer(&self) -> &[u8] { &[] }
}
