// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub use error::{EhciError, UsbError};

#[cfg(any(keyos, doc))]
pub mod device;
pub mod error;
#[cfg(any(keyos, doc))]
pub mod host;
