// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![feature(linked_list_cursors)]

pub use {
    response::Response,
    sleep::Sleep,
    subscription::Subscription,
    task::TaskHandle,
    timeout::{Timeout, TimeoutError},
    watch::StreamWatch,
};

mod async_watch;
mod implementation;
mod response;
mod sleep;
mod subscription;
mod task;
mod timeout;
mod watch;

use std::{future::IntoFuture, panic::Location, sync::Arc, time::Duration};

use server::{xous, AsyncMessageInit, EventSubscriptionMessage, MessageId, Owned};

use crate::implementation::{
    HandlerInit, Inner, RequestSlot, Slot, SubscriptionSlot, WorkerEvent, WorkerServer,
};

/// A cheaply clonable handle to the worker runtime
///
/// Dropping the last handle shuts down the worker thread
#[derive(Debug, Default, Clone)]
pub struct WorkerHandle {
    inner: Arc<Inner>,
}

impl WorkerHandle {
    /// Spawn a future onto the worker thread
    ///
    /// The returned handle can await the result or cancel the task
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # async fn test() {
    /// let worker = worker::WorkerHandle::default();
    /// let handle = worker.spawn(async {
    ///     println!("Hello from worker thread!");
    ///     42
    /// });
    ///
    /// // Wait for completion
    /// let result = handle.await;
    /// assert_eq!(result, 42);
    /// # }
    /// ```
    pub fn spawn<I, T>(&self, f: I) -> TaskHandle<T>
    where
        I: IntoFuture<Output = T>,
        <I as IntoFuture>::IntoFuture: Send + 'static,
        T: Send + 'static,
    {
        let shared = Arc::clone(&self.inner.shared);
        let schedule = move |task| {
            shared.queue_event(WorkerEvent::Task { task });
        };
        let (r, task) = async_task::spawn(f.into_future(), schedule);
        r.schedule();
        TaskHandle { task }
    }

    /// Return a future that completes after `duration`
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # async fn test() {
    /// let worker = worker::WorkerHandle::default();
    /// worker.sleep(std::time::Duration::from_millis(100)).await;
    /// println!("Slept for 100ms");
    /// # }
    /// ```
    pub fn sleep(&self, duration: Duration) -> Sleep {
        let handle = Arc::clone(&self.inner.shared);
        Sleep::new(duration, handle)
    }

    /// Run `future` until it completes or `duration` expires
    ///
    /// Returns `Err(TimeoutError)` if the timeout expires first
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # async fn test() {
    /// # use std::time::Duration;
    ///
    /// let worker = worker::WorkerHandle::default();
    /// let result = worker.timeout(
    ///     async {
    ///         // Some long operation
    ///         42
    ///     },
    ///     Duration::from_secs(5),
    /// ).await;
    ///
    /// match result {
    ///     Ok(value) => println!("Got result: {}", value),
    ///     Err(worker::TimeoutError) => println!("Operation timed out"),
    /// }
    /// # }
    /// ```
    pub fn timeout<F>(&self, future: F, duration: Duration) -> Timeout<F>
    where
        F: Future,
    {
        let handle = Arc::clone(&self.inner.shared);
        Timeout::new(future, duration, handle)
    }

    /// Start an infallible scalar subscription
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # mod mock { include!("mock.rs"); }
    /// # async fn test() {
    /// let worker = worker::WorkerHandle::default();
    /// let mut subscription = worker.subscribe_scalar::<mock::Permissions, _>(mock::ScalarSub);
    /// while let Some(event) = subscription.next().await {
    ///     // Handle event
    /// }
    /// # }
    /// ```
    #[track_caller]
    pub fn subscribe_scalar<P, M>(&self, sub: M) -> Subscription<M::Event>
    where
        M: server::ScalarSubscription<Error = server::Infallible> + Send + 'static,
        P: server::MessageAllowed<M>,
    {
        self.subscribe::<M, M::Event>(sub, subscribe_scalar_raw::<M>, server::decode_scalar_event::<M::Event>)
    }

    /// Start a scalar subscription and report setup errors
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # mod mock { include!("mock.rs"); }
    /// # async fn test() -> Result<(), server::Infallible> {
    /// let worker = worker::WorkerHandle::default();
    /// let mut subscription = worker.try_subscribe_scalar::<mock::Permissions, _>(mock::ScalarSub).await?;
    /// while let Some(event) = subscription.next().await {
    ///     // Handle event
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_subscribe_scalar<P, M>(&self, sub: M) -> TaskHandle<Result<Subscription<M::Event>, M::Error>>
    where
        M: server::ScalarSubscription + Send + 'static,
        M::Error: Send + 'static,
        M::Event: Send + 'static,
        P: server::MessageAllowed<M>,
    {
        self.try_subscribe::<M, M::Event, M::Error>(
            sub,
            subscribe_scalar_raw::<M>,
            server::decode_scalar_event::<M::Event>,
        )
    }

    /// Start an infallible archive subscription
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # mod mock { include!("mock.rs"); }
    /// # async fn test() {
    /// let worker = worker::WorkerHandle::default();
    /// let mut subscription = worker.subscribe_archive::<mock::Permissions, _>(mock::ArchiveSub);
    /// while let Some(event) = subscription.next().await {
    ///     // Handle event
    /// }
    /// # }
    /// ```
    pub fn subscribe_archive<P, M>(&self, sub: M) -> Subscription<M::Event>
    where
        M: server::ArchiveSubscription<Error = server::Infallible> + Send + 'static,
        P: server::MessageAllowed<M>,
    {
        self.subscribe::<M, M::Event>(
            sub,
            subscribe_archive_raw::<M>,
            server::decode_archive_event::<M::Event>,
        )
    }

    /// Start an archive subscription and report setup errors
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # mod mock { include!("mock.rs"); }
    /// # async fn test() -> Result<(), server::Infallible> {
    /// let worker = worker::WorkerHandle::default();
    /// let mut subscription = worker.try_subscribe_archive::<mock::Permissions, _>(mock::ArchiveSub).await?;
    /// while let Some(event) = subscription.next().await {
    ///     // Handle event
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_subscribe_archive<P, M>(&self, sub: M) -> TaskHandle<Result<Subscription<M::Event>, M::Error>>
    where
        M: server::ArchiveSubscription + Send + 'static,
        M::Error: Send + 'static,
        M::Event: Send + 'static,
        P: server::MessageAllowed<M>,
    {
        self.try_subscribe::<M, M::Event, M::Error>(
            sub,
            subscribe_archive_raw::<M>,
            server::decode_archive_event::<M::Event>,
        )
    }

    /// Send an archive request, panicking on transport or decode errors
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # mod mock { include!("mock.rs"); }
    /// # async fn test() {
    /// let worker = worker::WorkerHandle::default();
    /// let response = worker.async_archive::<mock::Permissions, _>(mock::ArchiveRequest { value: 42 }).await;
    /// # }
    /// ```
    #[track_caller]
    pub fn async_archive<P, M>(&self, msg: M) -> Response<M::Response>
    where
        M: server::BlockingArchive + Send + 'static,
        P: server::MessageAllowed<M>,
    {
        self.async_request(msg, send_blocking_archive_raw::<M>, |res, location| {
            let envelope = match res {
                Ok(envelope) => envelope,
                Err(e) => panic!("async_archive {location} {e:?}"),
            };
            match server::try_decode_archive_async_response::<M::Response>(envelope) {
                Ok(resp) => resp,
                Err(e) => panic!("async_archive decode {location} {e}"),
            }
        })
    }

    /// Send an archive request and return transport or decode errors in the response
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # mod mock { include!("mock.rs"); }
    /// # async fn test() -> Result<(), server::xous::Error> {
    /// let worker = worker::WorkerHandle::default();
    /// let response = worker.try_async_archive::<mock::Permissions, _>(mock::ArchiveRequest { value: 42 }).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_async_archive<P, M>(&self, msg: M) -> Response<Result<M::Response, xous::Error>>
    where
        M: server::BlockingArchive + Send + 'static,
        P: server::MessageAllowed<M>,
    {
        self.async_request(msg, send_blocking_archive_raw::<M>, |res, _| {
            let msg = res?;
            server::try_decode_archive_async_response::<M::Response>(msg)
                .map_err(|e| e.into_inner().into_xous())
        })
    }

    /// Send an archive request and return a zero-copy response view
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # mod mock { include!("mock.rs"); }
    /// # async fn test() -> Result<(), server::xous::Error> {
    /// let worker = worker::WorkerHandle::default();
    /// let response = worker.try_async_archive_ref::<mock::Permissions, _>(mock::ArchiveRequest { value: 42 }).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_async_archive_ref<P, M>(&self, msg: M) -> Response<Result<Owned<M::Response>, xous::Error>>
    where
        M: server::BlockingArchive + Send + 'static,
        P: server::MessageAllowed<M>,
    {
        self.async_request(msg, send_blocking_archive_raw::<M>, |res, _| {
            let envelope = res?;
            Owned::new_move(envelope).map_err(|_| xous::Error::InternalError)
        })
    }

    /// Send a scalar request, panicking on transport errors
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # mod mock { include!("mock.rs"); }
    /// # async fn test() {
    /// let worker = worker::WorkerHandle::default();
    /// let response = worker.async_scalar::<mock::Permissions, _>(mock::ScalarRequest(42)).await;
    /// # }
    /// ```
    #[track_caller]
    pub fn async_scalar<P, M>(&self, msg: M) -> Response<M::Response>
    where
        M: server::BlockingScalar + Send + 'static,
        P: server::MessageAllowed<M>,
    {
        self.async_request(msg, send_scalar_raw, |res, location| {
            let envelope = match res {
                Ok(envelope) => envelope,
                Err(e) => panic!("async_scalar {location} {e:?}"),
            };
            server::decode_scalar_async_response::<M::Response>(envelope)
        })
    }

    /// Send a scalar request and return transport errors in the response
    ///
    /// # Example
    ///
    /// ```rust no_run
    /// # mod mock { include!("mock.rs"); }
    /// # async fn test() -> Result<(), server::xous::Error> {
    /// let worker = worker::WorkerHandle::default();
    /// let response = worker.try_async_scalar::<mock::Permissions, _>(mock::ScalarRequest(42)).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_async_scalar<P, M>(&self, msg: M) -> Response<Result<M::Response, xous::Error>>
    where
        M: server::BlockingScalar + Send + 'static,
        P: server::MessageAllowed<M>,
    {
        self.async_request(msg, send_scalar_raw, |res, _| {
            let msg = res?;
            Ok(server::decode_scalar_async_response::<M::Response>(msg))
        })
    }

    pub fn watch_stream<S, T>(&self, sub: S, initial: T) -> StreamWatch<T>
    where
        T: Clone + Send + Sync + 'static,
        S: futures_lite::Stream + Send + 'static,
        S::Item: Into<T> + Send + 'static,
    {
        StreamWatch::from_stream(self, sub, initial)
    }
}

/// Spawn a non-`Send` future onto the worker thread
///
/// The returned handle can await the result or cancel the task.
///
/// # Panics
///
/// Panics if called from any thread other than the worker thread.
///
/// # Example
///
/// ```rust no_run
/// # async fn test() {
/// let worker = worker::WorkerHandle::default();
/// let handle = worker.spawn(async move {
///     let local = worker::spawn_local(async {
///         println!("Hello from local worker task!");
///         42
///     });
///
///     local.await
/// });
///
/// // Wait for completion
/// let result = handle.await;
/// assert_eq!(result, 42);
/// # }
/// ```
#[track_caller]
pub fn spawn_local<I, T>(f: I) -> TaskHandle<T>
where
    I: IntoFuture<Output = T>,
    <I as IntoFuture>::IntoFuture: 'static,
    T: 'static,
{
    let shared = implementation::current_worker_shared()
        .expect("worker::spawn_local must be called from the worker thread");

    let (r, task) = async_task::spawn_local(f.into_future(), move |task| {
        shared.queue_event(WorkerEvent::Task { task });
    });

    r.schedule();
    TaskHandle { task }
}

type SubInit<M, E> = fn(
    xous::CID,
    xous::CID,
    xous::MessageId,
    xous::MessageId,
    M,
) -> whence::Result<Result<(), E>, server::Error>;

type AsyncInit<M> = fn(xous::CID, xous::CID, xous::MessageId, M) -> whence::Result<(), server::Error>;

impl WorkerHandle {
    fn subscribe<M, Event>(
        &self,
        msg: M,
        sub: SubInit<M, server::Infallible>,
        decode: fn(xous::MessageEnvelope) -> Event,
    ) -> Subscription<Event>
    where
        M: MessageId + Send + 'static,
        Event: 'static,
    {
        let inner = &self.inner;
        let (cancel_token, cancel) = inner.next_cancel_token();
        let (tx, rx) = async_channel::bounded(10);

        let mut state = Some((msg, tx));
        let init = HandlerInit {
            cancel_token,
            init: Box::new(move |s: &mut WorkerServer| {
                let (cid, cid_remote, pid) = s.try_get_connection_from_name(M::SERVER).unwrap()?;
                let slot = s.reserve_slot();
                let (msg, tx) = state.take().unwrap();
                match sub(cid, cid_remote, slot.key.msg_id(), slot.key.cancel_msg_id(), msg).unwrap() {
                    Ok(()) => (),
                    Err(e) => match e {},
                }
                slot.insert(Slot::Subscription(SubscriptionSlot { tx, pid, cancel_token }));
                Some(())
            }),
        };

        inner.queue_event(WorkerEvent::Register { init });

        Subscription { rx, decode, cancel }
    }

    #[inline]
    fn try_subscribe<M, Event, Err>(
        &self,
        msg: M,
        sub: SubInit<M, Err>,
        decode: fn(xous::MessageEnvelope) -> Event,
    ) -> TaskHandle<Result<Subscription<Event>, Err>>
    where
        M: MessageId + Send + 'static,
        Err: Send + 'static,
        Event: Send + 'static,
    {
        let this = self.clone();
        self.spawn(async move {
            let inner = &this.inner;
            let (cancel_token, cancel) = inner.next_cancel_token();
            let (tx, rx) = async_channel::bounded(10);
            let (result_tx, result_rx) = oneshot::channel();

            let mut state = Some((msg, tx, result_tx));
            let init = HandlerInit {
                cancel_token,
                init: Box::new(move |s: &mut WorkerServer| {
                    let (cid, cid_remote, pid) = match s.try_get_connection_from_name(M::SERVER) {
                        Ok(ids) => ids?,
                        Err(e) => {
                            let (_, _, result_tx) = state.take().unwrap();
                            let _ = result_tx.send(Err(server::Error::from(e).into()));
                            return Some(());
                        }
                    };
                    let slot = s.reserve_slot();
                    let (msg, tx, result_tx) = state.take().unwrap();
                    let res = sub(cid, cid_remote, slot.key.msg_id(), slot.key.cancel_msg_id(), msg);
                    if let Ok(Ok(())) = res {
                        slot.insert(Slot::Subscription(SubscriptionSlot { tx, pid, cancel_token }));
                    }
                    let _ = result_tx.send(res);
                    Some(())
                }),
            };

            inner.queue_event(WorkerEvent::Register { init });
            result_rx.await.unwrap().unwrap()?;
            Ok(Subscription { rx, decode, cancel })
        })
    }

    #[inline]
    #[track_caller]
    fn async_request<M, R>(
        &self,
        msg: M,
        async_fn: AsyncInit<M>,
        decode: fn(Result<xous::MessageEnvelope, xous::Error>, &'static Location<'static>) -> R,
    ) -> Response<R>
    where
        M: MessageId + Send + 'static,
        R: 'static,
    {
        let inner = &self.inner;
        let (cancel_token, cancel) = inner.next_cancel_token();
        let location = Location::caller();
        let (tx, rx) = oneshot::channel();

        let mut state = Some((msg, tx));
        let init = HandlerInit {
            cancel_token,
            init: Box::new(move |s: &mut WorkerServer| {
                let (cid, cid_remote, pid) = match s.try_get_connection_from_name(M::SERVER) {
                    Ok(ids) => ids?,
                    Err(e) => {
                        let (_, tx) = state.take().unwrap();
                        let _ = tx.send(Err(e));
                        return Some(());
                    }
                };
                let slot = s.reserve_slot();
                let (msg, tx) = state.take().unwrap();
                match async_fn(cid, cid_remote, slot.key.msg_id(), msg) {
                    Ok(()) => {
                        slot.insert(Slot::Request(RequestSlot { tx: Some(tx), pid, cancel_token }));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.into_inner().into_xous()));
                    }
                }
                Some(())
            }),
        };

        inner.queue_event(WorkerEvent::Register { init });

        Response { rx, decode, location, cancel }
    }

    #[cfg(feature = "integration_test")]
    pub fn get_retry_timer_active(&self) -> bool {
        server::send_blocking_scalar(self.inner.shared.conn, implementation::GetRetryTimerActive)
    }
}

fn send_blocking_archive_raw<M: server::BlockingArchive>(
    cid: xous::CID,
    remote_cid: xous::CID,
    msg_id: xous::MessageId,
    msg: M,
) -> whence::Result<(), server::Error> {
    AsyncMessageInit { cid: remote_cid, msg_id, msg }.send_blocking_archive(cid)
}

fn send_scalar_raw<M: server::BlockingScalar>(
    cid: xous::CID,
    remote_cid: xous::CID,
    msg_id: xous::MessageId,
    msg: M,
) -> whence::Result<(), server::Error> {
    AsyncMessageInit { cid: remote_cid, msg_id, msg }.send_scalar(cid)
}

fn subscribe_scalar_raw<M: server::ScalarSubscription>(
    cid: xous::CID,
    remote_cid: xous::CID,
    msg_id: xous::MessageId,
    cancel_msg_id: xous::MessageId,
    msg: M,
) -> whence::Result<Result<(), M::Error>, server::Error> {
    EventSubscriptionMessage { cid: remote_cid, msg_id, cancel_msg_id, msg }.send_scalar(cid)
}

fn subscribe_archive_raw<M: server::ArchiveSubscription>(
    cid: xous::CID,
    remote_cid: xous::CID,
    msg_id: xous::MessageId,
    cancel_msg_id: xous::MessageId,
    msg: M,
) -> whence::Result<Result<(), M::Error>, server::Error> {
    EventSubscriptionMessage { cid: remote_cid, msg_id, cancel_msg_id, msg }.send_archive(cid)
}

/// Code from <https://doc.rust-lang.org/std/task/trait.Wake.html#examples>
#[cfg(any(test, feature = "test_executor"))]
pub mod test_executor {
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake};
    use std::thread::{self, Thread};
    use std::time::{Duration, Instant};

    /// A waker that wakes up the current thread when called.
    struct ThreadWaker(Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) { self.0.unpark(); }
    }

    /// Run a future to completion on the current thread.
    pub fn block_on<T>(fut: impl Future<Output = T>) -> T {
        // Pin the future so it can be polled.
        let mut fut = Box::pin(fut);

        // Create a new context to be passed to the future.
        let t = thread::current();
        let waker = Arc::new(ThreadWaker(t)).into();
        let mut cx = Context::from_waker(&waker);

        // Run the future to completion.
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(res) => return res,
                Poll::Pending => thread::park(),
            }
        }
    }

    pub fn block_timeout<T>(fut: impl Future<Output = T>, duration: Duration) -> Option<T> {
        let start = Instant::now();
        // Pin the future so it can be polled.
        let mut fut = Box::pin(fut);

        // Create a new context to be passed to the future.
        let t = thread::current();
        let waker = Arc::new(ThreadWaker(t)).into();
        let mut cx = Context::from_waker(&waker);

        // Run the future to completion.
        loop {
            let elapsed = start.elapsed();
            let remaining = duration.saturating_sub(elapsed);
            if remaining.is_zero() {
                return None;
            }
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(res) => return Some(res),
                Poll::Pending => thread::park_timeout(remaining),
            }
        }
    }
}
