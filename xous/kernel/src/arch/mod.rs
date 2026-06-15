// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

#[cfg(keyos)]
mod arm;
#[cfg(keyos)]
pub use crate::arch::arm::*;

#[cfg(any(windows, unix))]
mod hosted;
#[cfg(any(windows, unix))]
pub use hosted::*;
