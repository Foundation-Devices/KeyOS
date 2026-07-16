// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(dead_code)]

//! A single-producer, multi-consumer channel that only retains the latest value.

use event_listener::Event;

mod sync {
    #[cfg(not(all(test, loom)))]
    pub use std::sync::*;

    #[cfg(all(test, loom))]
    pub use loom::sync::*;
}

use sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, RwLock, RwLockReadGuard,
};

pub mod error {
    use std::fmt;

    /// Error produced when sending a value fails.
    #[derive(Debug)]
    pub struct SendError<T>(pub T);

    impl<T: fmt::Debug> fmt::Display for SendError<T> {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result { write!(fmt, "channel closed") }
    }

    impl<T: fmt::Debug> std::error::Error for SendError<T> {}

    /// Error produced when receiving a value fails.
    #[derive(Debug, Clone, Copy)]
    pub struct RecvError;

    impl fmt::Display for RecvError {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result { write!(fmt, "channel closed") }
    }

    impl std::error::Error for RecvError {}
}

const CLOSED_BIT: usize = 1;
const VERSION_STEP: usize = 2;

#[derive(Debug)]
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

#[derive(Debug)]
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    version: Version,
}

#[derive(Debug)]
pub struct Ref<'a, T> {
    inner: RwLockReadGuard<'a, T>,
}

#[derive(Debug)]
struct Shared<T> {
    value: RwLock<T>,
    state: AtomicState,
    ref_count_rx: AtomicUsize,
    ref_count_tx: AtomicUsize,
    event_value_changed: Event,
    event_all_recv_dropped: Event,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Version(usize);

#[derive(Clone, Copy, Debug)]
struct StateSnapshot(usize);

#[derive(Debug)]
struct AtomicState(AtomicUsize);

impl Version {
    const INITIAL: Self = Self(0);
}

impl StateSnapshot {
    fn version(self) -> Version { Version(self.0 & !CLOSED_BIT) }

    fn is_closed(self) -> bool { (self.0 & CLOSED_BIT) == CLOSED_BIT }
}

impl AtomicState {
    fn new() -> Self { Self(AtomicUsize::new(Version::INITIAL.0)) }

    fn load(&self) -> StateSnapshot { StateSnapshot(self.0.load(Ordering::Acquire)) }

    fn increment_version_while_locked(&self) { self.0.fetch_add(VERSION_STEP, Ordering::Release); }

    fn set_closed(&self) { self.0.fetch_or(CLOSED_BIT, Ordering::Release); }
}

pub fn channel<T>(init: T) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        value: RwLock::new(init),
        state: AtomicState::new(),
        ref_count_rx: AtomicUsize::new(1),
        ref_count_tx: AtomicUsize::new(1),
        event_value_changed: Event::new(),
        event_all_recv_dropped: Event::new(),
    });

    let tx = Sender { shared: shared.clone() };
    let rx = Receiver { shared, version: Version::INITIAL };

    (tx, rx)
}

impl<T> Receiver<T> {
    pub fn borrow(&self) -> Ref<'_, T> {
        let inner = self.shared.value.read().unwrap();
        Ref { inner }
    }

    pub fn borrow_and_update(&mut self) -> Ref<'_, T> {
        let inner = self.shared.value.read().unwrap();
        let new_version = self.shared.state.load().version();
        self.version = new_version;
        Ref { inner }
    }

    pub async fn changed(&mut self) -> Result<(), error::RecvError> {
        loop {
            let listener = self.shared.event_value_changed.listen();

            if let Some(ret) = maybe_changed(&self.shared, &mut self.version) {
                return ret;
            }

            listener.await;
        }
    }
}

impl<T: Clone> Receiver<T> {
    pub async fn recv(&mut self) -> Result<T, error::RecvError> {
        self.changed().await?;
        Ok(self.borrow_and_update().clone())
    }
}

fn maybe_changed<T>(shared: &Shared<T>, version: &mut Version) -> Option<Result<(), error::RecvError>> {
    let state = shared.state.load();
    let new_version = state.version();

    if *version != new_version {
        *version = new_version;
        return Some(Ok(()));
    }

    if state.is_closed() {
        return Some(Err(error::RecvError));
    }

    None
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.shared.ref_count_rx.fetch_add(1, Ordering::Relaxed);
        Self { shared: self.shared.clone(), version: self.version }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if self.shared.ref_count_rx.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.shared.event_all_recv_dropped.notify(usize::MAX);
        }
    }
}

impl<T> Sender<T> {
    pub fn send(&self, value: T) -> Result<(), error::SendError<T>> {
        if self.shared.ref_count_rx.load(Ordering::Relaxed) == 0 {
            return Err(error::SendError(value));
        }

        {
            let mut lock = self.shared.value.write().unwrap();
            *lock = value;
            self.shared.state.increment_version_while_locked();
        }

        self.shared.event_value_changed.notify(usize::MAX);

        Ok(())
    }

    pub async fn closed(&self) {
        loop {
            if self.shared.ref_count_rx.load(Ordering::Relaxed) == 0 {
                return;
            }

            let listener = self.shared.event_all_recv_dropped.listen();

            if self.shared.ref_count_rx.load(Ordering::Relaxed) == 0 {
                return;
            }

            listener.await;
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if self.shared.ref_count_tx.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.state.set_closed();
            self.shared.event_value_changed.notify(usize::MAX);
        }
    }
}

impl<T> std::ops::Deref for Ref<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target { self.inner.deref() }
}

#[cfg(test)]
fn now_or_never<F: Future>(future: F) -> Option<F::Output> {
    use std::task::{Context, Poll, Waker};
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let pin = std::pin::pin!(future);
    match pin.poll(&mut cx) {
        Poll::Ready(val) => Some(val),
        Poll::Pending => None,
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn changed_observes_send() {
        let (tx, mut rx) = channel("hello");
        assert!(now_or_never(rx.changed()).is_none());
        tx.send("world").unwrap();
        assert!(now_or_never(rx.changed()).is_some());
        assert_eq!(*rx.borrow(), "world");
    }
}

#[cfg(all(test, loom))]
mod loom_tests {
    use loom::{model, sync::Arc, thread};

    use super::*;

    fn check_model(f: impl Fn() + Sync + Send + 'static) {
        let mut builder = model::Builder::new();
        builder.preemption_bound = Some(3);
        builder.check(f);
    }

    #[test]
    fn changed_handles_spurious_wakeup_interleavings() {
        check_model(|| {
            let (send, mut recv) = channel(0i32);

            send.send(1).unwrap();

            let send_thread = thread::spawn(move || {
                send.send(2).unwrap();
                send
            });

            let _ = now_or_never(recv.changed());

            let send = send_thread.join().unwrap();
            let recv_thread = thread::spawn(move || {
                let _ = now_or_never(recv.changed());
                let _ = now_or_never(recv.changed());
                recv
            });

            send.send(3).unwrap();

            let mut recv = recv_thread.join().unwrap();
            let send_thread = thread::spawn(move || {
                send.send(2).unwrap();
            });

            let _ = now_or_never(recv.changed());

            send_thread.join().unwrap();
        });
    }

    #[test]
    fn recv_does_not_leave_latest_value_unseen() {
        check_model(|| {
            let (tx, mut rx) = channel(0i32);
            let tx = Arc::new(tx);

            tx.send(1).unwrap();

            let send = thread::spawn({
                let tx = tx.clone();
                move || {
                    tx.send(2).unwrap();
                }
            });

            let first = match now_or_never(rx.recv()) {
                Some(Ok(value)) => value,
                other => panic!("recv should be ready, got {other:?}"),
            };

            send.join().unwrap();

            let second = now_or_never(rx.changed());
            assert!(
                !(first == 2 && matches!(second, Some(Ok(())))),
                "recv() observed the latest value without marking it seen"
            );
        });
    }
}
