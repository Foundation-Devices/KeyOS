// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
use std::{
    future::{Future, IntoFuture},
    time::Duration,
};

pub(crate) mod core;
pub(crate) mod handle;
pub(crate) mod pixel;
pub(crate) mod stored_value;

#[doc(hidden)]
pub use core::Runtime;

pub use handle::{QueueEventError, RuntimeHandle};
pub use stored_value::{StoredRef, StoredRefMut, StoredValue};
pub use worker::{Response, Sleep, StreamWatch, Subscription, TaskHandle, Timeout, WorkerHandle};

/// Queue a task to be executed on the main thread.
///
/// This function can be called from a non-main thread. The task will be executed on the next
/// event loop tick of the main thread.
///
/// # Arguments
///
/// * `f` - A future or function that returns a future to be executed
///
/// # Returns
///
/// A [`TaskHandle<T>`] that can be used to cancel the task or wait for its completion.
///
/// # Example
///
/// ```rust no_run
/// # async fn test() {
/// let handle = slint_keyos_platform::spawn(async {
///     // This runs on the main thread
///     println!("Hello from main thread!");
///     42
/// });
///
/// // Wait for completion
/// let result = handle.await;
/// assert_eq!(result, 42);
/// # }
/// ```
pub fn spawn<I, T>(f: I) -> TaskHandle<T>
where
    I: IntoFuture<Output = T>,
    <I as IntoFuture>::IntoFuture: Send + 'static,
    T: Send + 'static,
{
    handle::global::handle().spawn(f)
}

/// Queue a task to be executed from the main thread.
///
/// Can only be called from the main thread. This is useful for spawning tasks that don't need
/// to be `Send` since they're already running on the main thread.
///
/// # Returns
///
/// A [`TaskHandle<T>`] that can be used to cancel the task or wait for its completion.
///
/// # Example
///
/// ```rust no_run
/// # async fn test() {
/// let handle = slint_keyos_platform::spawn_local(async {
///     // This runs on the main thread
///     println!("Hello from main thread!");
///     "local result"
/// });
///
/// // Wait for completion
/// let result = handle.await;
/// assert_eq!(result, "local result");
/// # }
/// ```
pub fn spawn_local<I, T>(f: I) -> TaskHandle<T>
where
    I: IntoFuture<Output = T>,
    <I as IntoFuture>::IntoFuture: 'static,
    T: 'static,
{
    core::with_runtime(|runtime| runtime.spawn_local(f))
}

/// Queue a task to be executed on a worker thread.
///
/// Can be called from any thread. The task will be executed on a dedicated worker thread,
/// making it suitable for CPU-intensive or blocking operations.
///
/// This task will be paused if the app is not visible
///
/// # Arguments
///
/// * `f` - A future or function that returns a future to be executed
///
/// # Returns
///
/// A [`TaskHandle<R>`] that can be used to cancel the task or wait for its completion.
///
/// # Example
///
/// ```rust no_run
/// # async fn test() {
/// let handle = slint_keyos_platform::spawn_worker(async {
///     println!("Hello from worker thread!");
///     "worker result"
/// });
///
/// // Wait for completion
/// let result = handle.await;
/// assert_eq!(result, "worker result");
/// # }
/// ```
pub fn spawn_worker<I, R>(f: I) -> TaskHandle<R>
where
    I: IntoFuture<Output = R>,
    <I as IntoFuture>::IntoFuture: Send + 'static,
    R: Send + 'static,
{
    handle::global::handle().spawn_worker(f)
}

/// Non-blocking sleep for a duration.
///
/// Yields control to the runtime for the specified duration without blocking the current thread.
///
/// # Arguments
///
/// * `duration` - The duration to sleep for
///
/// # Example
///
/// ```rust no_run
/// # async fn test() {
/// slint_keyos_platform::sleep(std::time::Duration::from_millis(100)).await;
/// println!("Slept for 100ms");
/// # }
/// ```
pub fn sleep(duration: Duration) -> Sleep { handle::global::handle().sleep(duration) }

/// Create a timeout for a future using the global runtime handle.
///
/// Wraps the provided future with a timeout, causing it to return an error if it doesn't
/// complete within the specified duration.
///
/// # Arguments
///
/// * `future` - The future to run with a timeout
/// * `duration` - Maximum time to wait for the future to complete
///
/// # Returns
///
/// A future that resolves to `Ok(T)` if the future completes in time,
/// or `Err(TimeoutError)` if the timeout expires first
///
/// # Example
///
/// ```rust no_run
/// # async fn test() {
/// use std::time::Duration;
///
/// let result = slint_keyos_platform::timeout(
///     async {
///         // Some long operation
///         42
///     },
///     Duration::from_secs(5)
/// ).await;
///
/// match result {
///     Ok(value) => println!("Got result: {}", value),
///     Err(_) => println!("Operation timed out"),
/// }
/// # }
/// ```
pub fn timeout<F>(future: F, duration: std::time::Duration) -> Timeout<F>
where
    F: Future,
{
    handle::global::handle().timeout(future, duration)
}

/// See [`WorkerHandle::subscribe_scalar`].
pub fn subscribe_scalar<P, M>(sub: M) -> Subscription<M::Event>
where
    M: server::ScalarSubscription<Error = server::Infallible> + Send + 'static,
    P: server::MessageAllowed<M>,
{
    handle::global::handle().worker().subscribe_scalar::<P, M>(sub)
}

/// See [`WorkerHandle::try_subscribe_scalar`].
pub fn try_subscribe_scalar<P, M>(sub: M) -> TaskHandle<Result<Subscription<M::Event>, M::Error>>
where
    M: server::ScalarSubscription + Send + 'static,
    M::Error: Send + 'static,
    M::Event: Send + 'static,
    P: server::MessageAllowed<M>,
{
    handle::global::handle().worker().try_subscribe_scalar::<P, M>(sub)
}

/// See [`WorkerHandle::subscribe_archive`].
pub fn subscribe_archive<P, M>(sub: M) -> Subscription<M::Event>
where
    M: server::ArchiveSubscription<Error = server::Infallible> + Send + 'static,
    P: server::MessageAllowed<M>,
{
    handle::global::handle().worker().subscribe_archive::<P, M>(sub)
}

/// See [`WorkerHandle::async_archive`].
#[track_caller]
pub fn async_archive<P, M>(msg: M) -> Response<M::Response>
where
    M: server::BlockingArchive + Send + 'static,
    P: server::MessageAllowed<M>,
{
    handle::global::handle().worker().async_archive::<P, M>(msg)
}

/// See [`WorkerHandle::try_async_archive`].
pub fn try_async_archive<P, M>(msg: M) -> Response<Result<M::Response, xous::Error>>
where
    M: server::BlockingArchive + Send + 'static,
    P: server::MessageAllowed<M>,
{
    handle::global::handle().worker().try_async_archive::<P, M>(msg)
}

/// See [`WorkerHandle::async_scalar`].
#[track_caller]
pub fn async_scalar<P, M>(msg: M) -> Response<M::Response>
where
    M: server::BlockingScalar + Send + 'static,
    P: server::MessageAllowed<M>,
{
    handle::global::handle().worker().async_scalar::<P, M>(msg)
}

/// See [`WorkerHandle::try_async_scalar`].
pub fn try_async_scalar<P, M>(msg: M) -> Response<Result<M::Response, xous::Error>>
where
    M: server::BlockingScalar + Send + 'static,
    P: server::MessageAllowed<M>,
{
    handle::global::handle().worker().try_async_scalar::<P, M>(msg)
}

/// Wake up the runtime.
///
/// # Example
///
/// ```rust no_run
/// # slint_keyos_platform::Runtime::unsafe_init(|| ());
/// std::thread::spawn(move || {
///     // wake up the runtime from a worker thread
///     slint_keyos_platform::wake_runtime();
/// })
/// .join()
/// .unwrap();
/// ```
pub fn wake_runtime() { handle::global::handle().wake(); }

/// Shutdown the runtime on the next event loop tick.
///
/// # Example
///
/// ```rust no_run
/// # slint_keyos_platform::Runtime::unsafe_init(|| ());
/// std::thread::spawn(move || {
///     slint_keyos_platform::quit_runtime();
/// })
/// .join()
/// .unwrap();
/// ```
pub fn quit_runtime() { handle::global::handle().quit(); }

/// Returns `true` if the current thread is the main runtime thread
pub fn is_main_thread() -> bool { core::is_main_thread() }

/// Returns a handle to the worker runtime
#[inline]
pub fn worker() -> &'static WorkerHandle { handle::global::handle().worker() }
