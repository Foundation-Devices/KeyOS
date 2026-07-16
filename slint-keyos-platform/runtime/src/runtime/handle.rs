// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    future::{Future, IntoFuture},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, TrySendError},
        Arc, OnceLock,
    },
    time::Duration,
};

use slint::platform::EventLoopProxy;
use worker::{Sleep, TaskHandle, Timeout, WorkerHandle};

use crate::runtime::core::{with_runtime, EventLoopWaker, MainEvent};

#[derive(Clone)]
pub struct RuntimeHandle {
    pub(crate) inner: Arc<RuntimeHandleInner>,
}

impl RuntimeHandle {
    pub fn default() -> Self { with_runtime(|runtime| runtime.handle()) }

    pub fn spawn<I, T>(&self, task: I) -> TaskHandle<T>
    where
        I: IntoFuture<Output = T>,
        <I as IntoFuture>::IntoFuture: Send + 'static,
        T: Send + 'static,
    {
        let handle = self.clone();
        let (runnable, task) = async_task::spawn(task.into_future(), move |r| {
            handle.queue_event(MainEvent::Task(r));
        });
        runnable.schedule();
        TaskHandle::from(task)
    }

    pub fn spawn_worker<I, T>(&self, task: I) -> TaskHandle<T>
    where
        I: IntoFuture<Output = T>,
        <I as IntoFuture>::IntoFuture: Send + 'static,
        T: Send + 'static,
    {
        self.worker().spawn(task)
    }

    pub fn sleep(&self, duration: Duration) -> Sleep { self.worker().sleep(duration) }

    pub fn timeout<F>(&self, future: F, duration: Duration) -> Timeout<F>
    where
        F: Future,
    {
        self.worker().timeout(future, duration)
    }

    pub fn quit(&self) { self.queue_event(MainEvent::Quit) }

    pub fn wake(&self) { (self.inner.waker)(); }

    pub(crate) fn worker(&self) -> &WorkerHandle { self.inner.worker.get_or_init(WorkerHandle::default) }
}

impl RuntimeHandle {
    pub(crate) fn queue_event(&self, event: MainEvent) {
        if let Err(e) = self.try_queue_event(event) {
            log::error!("failed to queue event: {e}");
        }
    }

    pub(crate) fn try_queue_event(&self, event: MainEvent) -> Result<(), QueueEventError> {
        self.inner.main_tx.try_send(event)?;

        // Send a wake if necessary
        if !self.inner.wake_sent.swap(true, Ordering::SeqCst) {
            (self.inner.waker)();
        }

        Ok(())
    }
}

pub(crate) struct RuntimeHandleInner {
    // Guard to only send a wake message once per run()
    pub(crate) wake_sent: AtomicBool,
    // Waker to wake up the runtime
    pub(crate) waker: Arc<dyn EventLoopWaker>,
    pub(crate) main_tx: SyncSender<MainEvent>,
    pub(crate) worker: OnceLock<WorkerHandle>,
}

impl EventLoopProxy for RuntimeHandle {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        Ok(self.try_queue_event(MainEvent::Quit)?)
    }

    fn invoke_from_event_loop(&self, task: Box<dyn FnOnce() + Send>) -> Result<(), slint::EventLoopError> {
        self.spawn(async move { task() }).detach();
        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum QueueEventError {
    #[error("the event queue is full. this is a bug, please report it.")]
    Full,
    #[error("runtime has already been disposed of")]
    Disconnected,
}

impl From<QueueEventError> for slint::EventLoopError {
    fn from(e: QueueEventError) -> Self {
        match e {
            QueueEventError::Full => slint::EventLoopError::NoEventLoopProvider,
            QueueEventError::Disconnected => slint::EventLoopError::EventLoopTerminated,
        }
    }
}
impl From<TrySendError<MainEvent>> for QueueEventError {
    fn from(e: TrySendError<MainEvent>) -> Self {
        match e {
            TrySendError::Full(_) => QueueEventError::Full,
            TrySendError::Disconnected(_) => QueueEventError::Disconnected,
        }
    }
}

// static handle for apps to have convenient access to the runtime
pub(crate) mod global {
    use std::sync::LazyLock;

    use super::*;

    static RUNTIME_HANDLE: LazyLock<RuntimeHandle> = LazyLock::new(RuntimeHandle::default);

    // this needs to be called from main thread
    pub(crate) fn init() { let _ = LazyLock::force(&RUNTIME_HANDLE); }

    #[inline]
    pub(crate) fn handle() -> &'static RuntimeHandle { &RUNTIME_HANDLE }
}
