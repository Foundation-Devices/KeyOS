// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use crate::implementation::Shared;
use crate::sleep::Sleep;

pin_project_lite::pin_project! {
    pub struct Timeout<F> {
        #[pin]
        future: F,
        #[pin]
        sleep: Sleep,
    }
}

impl<F> Timeout<F> {
    pub fn new(future: F, duration: Duration, handle: Arc<Shared>) -> Self {
        Self { future, sleep: Sleep::new(duration, handle) }
    }
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, TimeoutError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        // first check future
        if let Poll::Ready(output) = this.future.poll(cx) {
            return Poll::Ready(Ok(output));
        }

        // then check if we've timed out
        if this.sleep.poll(cx) == Poll::Ready(()) {
            return Poll::Ready(Err(TimeoutError));
        }

        // neither completed yet
        Poll::Pending
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutError;

impl std::fmt::Display for TimeoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "operation timed out") }
}

impl std::error::Error for TimeoutError {}
