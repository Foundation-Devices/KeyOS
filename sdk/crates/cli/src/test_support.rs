// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::sync::Mutex;

use once_cell::sync::Lazy;

pub(crate) static PROCESS_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
