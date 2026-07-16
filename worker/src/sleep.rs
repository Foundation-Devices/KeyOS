// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use crate::implementation::{Shared, Timer, WorkerEvent};

#[must_use]
pub struct Sleep {
    expires_at: Instant,
    handle: Option<Arc<Shared>>,
}

impl Sleep {
    pub(crate) fn new(duration: Duration, handle: Arc<Shared>) -> Self {
        Self { expires_at: Instant::now() + duration, handle: Some(handle) }
    }
}

impl std::future::Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.expires_at <= Instant::now() {
            return Poll::Ready(());
        }

        if let Some(handle) = self.handle.take() {
            handle.queue_event(WorkerEvent::Timer {
                timer: Timer { instant: self.expires_at, waker: cx.waker().clone() },
            });
        }

        Poll::Pending
    }
}
