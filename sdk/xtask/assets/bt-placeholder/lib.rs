// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! SDK-only compatibility surface for `quantum-link`.
//!
//! The real `bt` API is intentionally not shipped. `error.rs` is copied from
//! the selected KeyOS source so `BluetoothError` remains wire-compatible until
//! QuantumLink v2 removes the dependency.

extern crate self as gpio;
extern crate self as spi;

pub mod error;

pub use error::BluetoothError;

// `sdk/xtask build` appends wire-compatible GpioApiError and SpiError
// definitions generated from the selected KeyOS source checkout. Keeping them
// out of this static template ensures KEYOS_DIR cannot pair a copied
// BluetoothError with payload enums from a different KeyOS revision.
