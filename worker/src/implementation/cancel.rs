// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Weak;

use super::{Inner, WorkerEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelToken(pub u64);

impl CancelToken {
    pub const DISARMED: Self = Self(0);
    pub const START: Self = Self(1);
}

pub struct CancelOnDrop {
    worker: Weak<Inner>,
    token: CancelToken,
}

impl CancelOnDrop {
    pub fn new(worker: Weak<Inner>, token: CancelToken) -> Self { Self { worker, token } }

    #[inline]
    pub fn disarm(&mut self) { self.token = CancelToken::DISARMED; }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.token == CancelToken::DISARMED {
            return;
        }
        if let Some(worker) = self.worker.upgrade() {
            worker.queue_event(WorkerEvent::Cancel { token: self.token });
        }
    }
}
