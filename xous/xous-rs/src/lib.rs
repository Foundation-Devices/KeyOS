#![cfg_attr(any(target_os = "none", keyos), no_std)]

pub mod arch;

pub mod carton;
pub mod definitions;

pub mod drop_deallocate;
pub mod process;
pub mod string;
pub mod syscall;

pub use arch::{ProcessArgs, ProcessInit, ProcessStartup, ThreadInit};
pub use definitions::*;
pub use drop_deallocate::*;
#[cfg(keyos)]
pub use keyos;
pub use string::*;
pub use syscall::*;

#[cfg(feature = "processes-as-threads")]
pub use crate::arch::ProcessArgsAsThread;
