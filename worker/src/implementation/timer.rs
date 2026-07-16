// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Instant;

#[derive(Debug)]
pub struct Timer {
    pub instant: Instant,
    pub waker: std::task::Waker,
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool { self.instant == other.instant }
}

impl Eq for Timer {}

impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

impl Ord for Timer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering { other.instant.cmp(&self.instant) }
}
