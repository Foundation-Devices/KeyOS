// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use futures_lite::{future, Stream, StreamExt};

use crate::WorkerHandle;

/// a clonable handle to a stream that retains the latest value
#[derive(Clone)]
pub struct StreamWatch<T> {
    rx: crate::async_watch::Receiver<T>,
}

impl<T> StreamWatch<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub fn from_stream<S>(worker: &WorkerHandle, sub: S, initial: T) -> Self
    where
        S: Stream + Send + 'static,
        S::Item: Into<T> + Send + 'static,
    {
        let (tx, rx) = crate::async_watch::channel(initial);
        worker
            .spawn(async move {
                let mut sub = std::pin::pin!(sub);
                loop {
                    let next = future::or(
                        async {
                            tx.closed().await;
                            None
                        },
                        sub.next(),
                    )
                    .await;

                    match next {
                        Some(event) => {
                            if tx.send(event.into()).is_err() {
                                return;
                            }
                        }
                        None => return,
                    }
                }
            })
            .detach();
        Self { rx }
    }

    /// wait until a predicate is satisfied.
    pub async fn wait_until(&self, mut predicate: impl FnMut(&T) -> bool) {
        let mut rx = self.rx.clone();
        loop {
            if predicate(&*rx.borrow()) {
                return;
            }
            // wait for next value change
            if rx.changed().await.is_err() {
                // sender dropped
                return;
            }
        }
    }

    /// get the current value
    #[inline]
    pub fn current(&self) -> T { (*self.rx.borrow()).clone() }

    /// borrow the current value
    #[inline]
    pub fn borrow(&self) -> crate::async_watch::Ref<'_, T> { self.rx.borrow() }

    /// wait for the next event
    #[inline]
    pub async fn next(&mut self) -> Option<T> { self.rx.recv().await.ok() }

    pub fn into_stream(self) -> impl Stream<Item = T> {
        futures_lite::stream::unfold(self.rx, |mut rx| async move { rx.recv().await.ok().map(|v| (v, rx)) })
    }
}
