// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::future::IntoFuture;
use std::{
    cell::RefCell,
    sync::{
        atomic::AtomicBool,
        mpsc::{Receiver, TryRecvError},
        Arc,
    },
};

use async_task::Runnable;
use slotmap::SlotMap;

use super::RuntimeHandle;
use crate::handle::RuntimeHandleInner;
use crate::TaskHandle;

/// Wake up the event loop
#[doc(hidden)]
pub trait EventLoopWaker: Send + Sync + Fn() {}
impl<T> EventLoopWaker for T where T: Send + Sync + Fn() {}

pub struct Runtime {
    pub(crate) state: RefCell<RuntimeState>,
    pub(crate) stored: RefCell<SlotMap<StoredValueKey, InnerStoredValue>>,
    pub(crate) handle: RuntimeHandle,
}

// inner mutable runtime state
pub(crate) struct RuntimeState {
    is_quit: bool,
    rx: Receiver<MainEvent>,
}

slotmap::new_key_type! { pub(crate) struct StoredValueKey; }

thread_local! {
    // Runtime can only accessed from the main thread
    static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

// run the closure only on the main thread
// only if the runtime is initialized
pub(crate) fn try_with_runtime<R>(f: impl FnOnce(&Runtime) -> R) -> Option<R> {
    RUNTIME.with(|runtime| {
        let runtime = runtime.borrow();
        let runtime = runtime.as_ref()?;
        Some(f(runtime))
    })
}

#[track_caller]
pub(crate) fn with_runtime<R>(f: impl FnOnce(&Runtime) -> R) -> R {
    try_with_runtime(f).expect("runtime exists")
}

pub(crate) fn is_main_thread() -> bool { RUNTIME.with(|runtime| runtime.borrow().is_some()) }

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum EventLoopStatus {
    // runtime is quitting
    Quit,
    // continue running the event loop
    Continue,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("Runtime already initialized")]
    AlreadyInitialized,
}

// crate public functions
impl Runtime {
    /// Runs the event loop until there are no more events to process, or until loop is quit
    ///
    /// # Panics
    ///
    /// - If not called from the main thread
    /// - If the runtime hasn't been initialized with [Runtime::initialize]
    pub fn unsafe_run() -> EventLoopStatus { with_runtime(|runtime| runtime.run()) }

    pub fn unsafe_init(waker: impl EventLoopWaker + 'static) {
        Runtime::try_init(waker).expect("initialize runtime");
    }

    /// Initializes the runtime
    ///
    /// Must be called before using the runtime.
    /// Returns an error if already initialized
    pub fn try_init(waker: impl EventLoopWaker + 'static) -> Result<(), RuntimeError> {
        let (tx, rx) = std::sync::mpsc::sync_channel(256);

        let runtime = Runtime {
            state: RefCell::new(RuntimeState { is_quit: false, rx }),
            stored: RefCell::new(SlotMap::with_capacity_and_key(32)),
            handle: RuntimeHandle {
                inner: Arc::new(RuntimeHandleInner {
                    wake_sent: AtomicBool::new(false),
                    waker: Arc::new(waker),
                    main_tx: tx,
                    worker: Default::default(),
                }),
            },
        };

        RUNTIME.with(|r| {
            let mut r = r.borrow_mut();
            if r.is_some() {
                Err(RuntimeError::AlreadyInitialized)
            } else {
                *r = Some(runtime);
                Ok(())
            }
        })?;

        Ok(())
    }

    pub fn unsafe_quit() { with_runtime(|runtime| runtime.handle.quit()) }

    /// Returns a handle to the runtime
    pub fn unsafe_handle() -> RuntimeHandle { RuntimeHandle::default() }
}

impl Runtime {
    #[inline]
    pub(crate) fn run(&self) -> EventLoopStatus {
        let mut state = self.state.borrow_mut();
        let status = state.run();
        // We processed all events, from now on let's request a wake message on any update.
        self.handle.inner.wake_sent.store(false, std::sync::atomic::Ordering::SeqCst);
        status
    }

    #[inline]
    pub(crate) fn spawn_local<I, T>(&self, runnable: I) -> TaskHandle<T>
    where
        I: IntoFuture<Output = T>,
        <I as IntoFuture>::IntoFuture: 'static,
        T: 'static,
    {
        let handle = self.handle();
        let (runnable, task) = async_task::spawn_local(runnable.into_future(), move |r| {
            handle.queue_event(MainEvent::Task(r));
        });
        runnable.schedule();
        TaskHandle::from(task)
    }

    pub(crate) fn handle(&self) -> RuntimeHandle { self.handle.clone() }
}

impl RuntimeState {
    #[inline]
    fn run(&mut self) -> EventLoopStatus {
        let is_quit = loop {
            let event = match self.rx.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Disconnected) => break true,
                Err(TryRecvError::Empty) => break false,
            };

            match event {
                MainEvent::Quit => break true,
                MainEvent::Task(runnable) => {
                    runnable.run();
                }
            }
        };

        // if runtime is already quit, preserve the quit state
        self.is_quit = self.is_quit || is_quit;

        if self.is_quit {
            EventLoopStatus::Quit
        } else {
            EventLoopStatus::Continue
        }
    }
}

#[derive(Debug)]
pub(crate) enum MainEvent {
    Quit,
    Task(Runnable<()>),
}

// don't make this copy/clonable
// that way we can guarantee we are only freeing it once
pub(crate) struct InnerStoredValue {
    value: *mut RefCell<dyn std::any::Any>,
    #[cfg(debug_assertions)]
    debug: ValueDebug,
}

impl InnerStoredValue {
    #[track_caller]
    pub(crate) fn new<T: 'static>(value: T) -> StoredValueKey {
        let value = Self {
            value: Box::into_raw(Box::new(RefCell::new(value))),
            #[cfg(debug_assertions)]
            debug: ValueDebug::new::<T>(),
        };

        with_runtime(|runtime| runtime.stored.borrow_mut().insert(value))
    }

    pub(crate) fn downcast_mut<T: 'static>(&self) -> Option<std::cell::RefMut<'static, T>> {
        // Safety: We know the pointer is valid because we created it in `new`
        let value = unsafe { &*self.value }.try_borrow_mut().ok()?;
        let result = std::cell::RefMut::map(value, |value| match value.downcast_mut() {
            Some(value) => value,
            None => {
                #[cfg(debug_assertions)]
                panic!(
                    "stored value defined at {} not of expected type `{}`. found `{}`",
                    self.debug.defined_at,
                    std::any::type_name::<T>(),
                    self.debug.ty_name
                );
                #[cfg(not(debug_assertions))]
                panic!("stored value not of expected type {}", std::any::type_name::<T>());
            }
        });
        Some(result)
    }

    pub(crate) fn downcast<T: 'static>(&self) -> Option<std::cell::Ref<'static, T>> {
        // Safety: We know the pointer is valid because we created it in `new`
        let value = unsafe { &*self.value }.try_borrow().ok()?;
        let result = std::cell::Ref::map(value, |value| match value.downcast_ref() {
            Some(value) => value,
            None => {
                #[cfg(debug_assertions)]
                panic!(
                    "stored value defined at {} not of expected type `{}`. found `{}`",
                    self.debug.defined_at,
                    std::any::type_name::<T>(),
                    self.debug.ty_name
                );
                #[cfg(not(debug_assertions))]
                panic!("stored value not of expected type {}", std::any::type_name::<T>());
            }
        });
        Some(result)
    }
}

impl Drop for InnerStoredValue {
    fn drop(&mut self) {
        // Safety: We know the pointer is valid because we created it in `new`
        let value = unsafe { Box::from_raw(self.value) };
        #[cfg(debug_assertions)]
        {
            if value.try_borrow_mut().is_err() {
                eprintln!("WARNING: dropping a StoredValue that is still borrowed");
            }
        }
        drop(value);
    }
}

/// Debug information for stored value
#[cfg(debug_assertions)]
#[derive(Copy, Clone)]
struct ValueDebug {
    defined_at: &'static std::panic::Location<'static>,
    ty_name: &'static str,
}

#[cfg(debug_assertions)]
impl ValueDebug {
    #[track_caller]
    fn new<T>() -> Self {
        let defined_at = std::panic::Location::caller();
        let ty_name = std::any::type_name::<T>();
        ValueDebug { defined_at, ty_name }
    }
}

/// Marker to ensure value is only accessed on the main thread
/// Not Send nor Sync
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MainThreadMarker(std::marker::PhantomData<*const ()>);

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;
    use crate::{spawn_local, StoredValue};

    struct TestRuntime {
        control_tx: Sender<(TestRequest, Sender<TestResponse>)>,
        handle: RuntimeHandle,
    }

    enum TestRequest {
        RunTick,
        Stop,
    }

    struct TestResponse {
        status: EventLoopStatus,
    }

    impl TestRuntime {
        fn new() -> Self {
            let (control_tx, control_rx) = channel::<(TestRequest, Sender<TestResponse>)>();

            // Start runtime in dedicated thread
            let (handle_tx, handle_rx) = channel();
            let _thread = thread::spawn(move || {
                Runtime::unsafe_init(|| ());
                let handle = Runtime::unsafe_handle();
                handle_tx.send(handle).expect("send handle");

                while let Ok((cmd, response_tx)) = control_rx.recv() {
                    match cmd {
                        TestRequest::RunTick => {
                            let status = with_runtime(|runtime| runtime.run());
                            let _ = response_tx.send(TestResponse { status });
                        }
                        TestRequest::Stop => break,
                    }
                }
            });

            let handle = handle_rx.recv().expect("receive handle");
            Self { control_tx, handle }
        }

        fn run_tick(&self) -> EventLoopStatus {
            let (tx, rx) = channel();
            self.control_tx.send((TestRequest::RunTick, tx)).expect("send tick command");
            rx.recv().expect("receive tick completion").status
        }
    }

    impl Drop for TestRuntime {
        fn drop(&mut self) {
            let (tx, _rx) = channel();
            let _ = self.control_tx.send((TestRequest::Stop, tx));
        }
    }

    #[test]
    fn test_nested_task_execution_order() {
        let runtime = TestRuntime::new();
        let handle = runtime.handle.clone();
        let order = Arc::new(Mutex::new(Vec::new()));

        let task = handle.spawn({
            let handle = handle.clone();
            let order = order.clone();
            async move {
                println!("task 1");
                order.lock().unwrap().push(1);
                handle
                    .spawn({
                        let handle = handle.clone();
                        let order = order.clone();
                        async move {
                            println!("task 2");
                            order.lock().unwrap().push(2);
                            handle
                                .spawn({
                                    let order = order.clone();
                                    async move {
                                        println!("task 3");
                                        order.lock().unwrap().push(3);
                                    }
                                })
                                .await;
                        }
                    })
                    .await;
            }
        });

        let task = handle.spawn({
            let handle = handle.clone();
            let order = order.clone();
            async move {
                task.await;
                println!("task 4");
                order.lock().unwrap().push(4);
                handle
                    .spawn({
                        let handle = handle.clone();
                        let order = order.clone();
                        async move {
                            println!("task 5");
                            order.lock().unwrap().push(5);
                            handle
                                .spawn({
                                    let order = order.clone();
                                    async move {
                                        println!("task 6");
                                        order.lock().unwrap().push(6);
                                    }
                                })
                                .await;
                        }
                    })
                    .await;
            }
        });

        while !task.is_finished() {
            runtime.run_tick();
        }

        assert_eq!(order.lock().unwrap().as_slice(), &[1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn test_spawn_cancel_on_drop() {
        let runtime = TestRuntime::new();

        let flag = Arc::new(AtomicBool::new(false));
        let handle = runtime.handle.spawn({
            let flag = flag.clone();
            async move {
                flag.store(true, Ordering::SeqCst);
            }
        });
        drop(handle);
        runtime.run_tick();
        assert!(!flag.load(Ordering::SeqCst));
        runtime.run_tick();
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn spawn_local_from_non_main_thread() {
        let result = std::panic::catch_unwind(|| {
            let _a = spawn_local(async { 1 });
        });

        assert!(result.is_err());
    }

    //
    // LOCAL TASK
    //

    fn init_local() { let _ = Runtime::try_init(|| ()); }

    #[test]
    fn example_borrow_vs_with() {
        init_local();
        let a = StoredValue::new(0);
        let b = StoredValue::new("yes".to_string());

        // nested closures can become unwieldy
        a.with(|a| {
            b.with(|b| {
                *a += 1;
                *b = "no".to_string();
            });
        });

        assert_eq!(a.get(), 1);
        assert_eq!(b.get(), "no");

        // these should be equivalent.
        {
            let mut a = a.borrow_mut();
            let mut b = b.borrow_mut();
            *a += 1;
            *b = "yes".to_string();
        }

        assert_eq!(a.get(), 2);
        assert_eq!(b.get(), "yes");
    }

    #[test]
    fn stored_value_borrow() {
        init_local();

        let value = StoredValue::new(0);
        let mut s = value.borrow_mut();
        assert_eq!(*s, 0);
        *s = 1;
        assert_eq!(*s, 1);
    }

    #[test]
    fn drop_runtime() {
        init_local();
        let value = StoredValue::new(0);
        RUNTIME.with(|runtime| {
            runtime.borrow_mut().take();
        });

        let result = value.try_borrow_mut().unwrap_err();
        assert_eq!(result, crate::stored_value::StoredValueError::RuntimeNotFound);
    }

    #[test]
    fn drop_stored_value() {
        init_local();

        let stored = StoredValue::new(42);

        {
            // Drop the stored value map
            // this should error print in debug mode
            RUNTIME.with(|runtime| {
                let runtime = runtime.borrow();
                let runtime = runtime.as_ref().unwrap();
                runtime.stored.borrow_mut().clear();
            });
        }

        let error = stored.try_borrow().unwrap_err();
        assert_eq!(error, crate::stored_value::StoredValueError::NotFound);
    }

    #[test]
    fn double_borrow_panic() {
        init_local();

        // Create a stored value
        let value = StoredValue::new(42);

        let a = value.borrow();
        let b = value.borrow();

        assert_eq!(*a, 42);
        assert_eq!(*b, 42);

        let result = std::panic::catch_unwind(|| {
            let _a = value.borrow_mut();
        });

        assert!(result.is_err());
    }
}
