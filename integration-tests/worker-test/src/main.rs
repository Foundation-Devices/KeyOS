// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod test_server;

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use disposable_server::{DisposableHandle, DisposablePermissions};
use futures_lite::{FutureExt, StreamExt};
use keyos_integration_test::{assert, assert_eq, fail};
use worker::{test_executor, WorkerHandle};
use worker_test_disposable as disposable_server;

use crate::test_server::{api::TestData, *};

async fn test_basic_task_spawning(worker: &WorkerHandle) {
    let task1 = worker.spawn(async { 42 });
    let task2 = worker.spawn(async { 43 });

    assert_eq!(task1.await, 42);
    assert_eq!(task2.await, 43);
}

async fn test_worker_sleep(worker: &WorkerHandle) {
    let start = Instant::now();

    let sleep2 = worker.spawn({
        let sleep = worker.sleep(Duration::from_millis(200));
        async move {
            sleep.await;
            log::info!("waking task 2");
            2
        }
    });

    let sleep1 = worker.spawn({
        let sleep = worker.sleep(Duration::from_millis(100));
        async move {
            sleep.await;
            log::info!("waking task 1");
            1
        }
    });

    let result = sleep1.race(sleep2).await;
    let elapsed = start.elapsed();
    // Hosted CI can introduce scheduler jitter; keep a bound that is strict enough
    // to catch regressions without flaking on normal contention.
    assert!(
        elapsed >= Duration::from_millis(100) && elapsed < Duration::from_millis(250),
        "elapsed {elapsed:?}"
    );
    assert_eq!(result, 1);

    // duration 0 doesn't break anything
    worker
        .spawn({
            let sleep = worker.sleep(Duration::from_nanos(100));
            async move {
                sleep.await;
                0
            }
        })
        .await;
}

async fn test_timeout_completes_before_timeout(worker: &WorkerHandle) {
    let result = worker
        .timeout(
            async {
                worker.sleep(Duration::from_millis(50)).await;
                42
            },
            Duration::from_millis(200),
        )
        .await;

    assert_eq!(result, Ok(42), "timeout should complete successfully when future finishes before timeout");
}

async fn test_timeout_expires(worker: &WorkerHandle) {
    let start = Instant::now();
    let result = worker
        .timeout(
            async {
                // never completes
                std::future::pending::<i32>().await
            },
            Duration::from_millis(100),
        )
        .await;
    let elapsed = start.elapsed();

    assert_eq!(
        result,
        Err(worker::TimeoutError),
        "timeout should expire when future doesn't complete in time"
    );

    assert!(
        elapsed >= Duration::from_millis(100) && elapsed < Duration::from_millis(250),
        "timeout elapsed {elapsed:?}"
    );
}

async fn test_scalar_subscription(worker: &WorkerHandle) {
    let sub_task = worker.spawn({
        let mut ticks = worker.subscribe_scalar::<TestPermissions, _>(api::ScalarTickSub);
        async move {
            let mut events = Vec::new();
            for _ in 0..5 {
                if let Some(event) = ticks.next().await {
                    events.push(event);
                }
            }
            events
        }
    });

    for _ in 0..5 {
        let _ = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;
    }

    let events = sub_task.await;
    assert_eq!(events.len(), 5, "should receive 5 tick events");
}

async fn test_archive_subscription(worker: &WorkerHandle) {
    let sub_task = worker.spawn({
        let mut counter_events = worker.subscribe_archive::<TestPermissions, _>(api::ArchiveTickSub);
        async move {
            let mut events = Vec::new();
            for _ in 0..5 {
                if let Some(event) = counter_events.next().await {
                    events.push(event);
                }
            }
            events
        }
    });

    for _ in 0..5 {
        let _ = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;
    }

    let events = sub_task.await;
    assert_eq!(events.len(), 5, "should receive 5 tick events");
}

async fn test_subscription_cancellation(worker: &WorkerHandle) {
    let scalar_ticks = worker.subscribe_scalar::<TestPermissions, _>(api::ScalarTickSub);
    let archive_ticks = worker.subscribe_archive::<TestPermissions, _>(api::ArchiveTickSub);

    let scalar_task = worker.spawn(async move { scalar_ticks.fold(0, |acc, _| acc + 1).await });

    let archive_task = worker.spawn(async move { archive_ticks.fold(0, |acc, _| acc + 1).await });

    // Send some ticks
    for _ in 0..3 {
        let _ = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;
    }

    let drop_response = worker.async_archive::<TestPermissions, _>(api::ArchiveDropAllSubs).await;
    assert!(drop_response.dropped_count > 0, "should have dropped some subscribers");

    // Send more ticks (these should not be received)
    for _ in 0..3 {
        let _ = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;
    }

    let scalar_count = scalar_task.await;
    let archive_count = archive_task.await;

    assert_eq!(scalar_count, 3, "should receive exactly 3 scalar events before cancellation");
    assert_eq!(archive_count, 3, "should receive exactly 3 archive events before cancellation");
}

async fn test_subscription_drops_old_events(worker: &WorkerHandle) {
    let mut ticks = worker.subscribe_scalar::<TestPermissions, _>(api::ScalarTickSub);
    let mut sent_ticks = Vec::new();

    for _ in 0..15 {
        sent_ticks.push(worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await.tick);
    }

    let mut received_ticks = Vec::new();
    for _ in 0..10 {
        let event = worker
            .timeout(ticks.next(), Duration::from_secs(1))
            .await
            .expect("subscription should have queued events")
            .expect("subscription should still be open");
        received_ticks.push(event.0);
    }

    assert_eq!(&received_ticks, &sent_ticks[5..], "subscription should keep the latest queued events");
}

async fn test_archive_async(worker: &WorkerHandle) {
    // we can have 16 in flight messages at the same time.
    let pending =
        (0..15).map(|_| worker.async_archive::<TestPermissions, _>(api::ArchiveTick)).collect::<Vec<_>>();
    let tick = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;

    // this req will not be sent until a slot frees up
    let waiting = worker.async_archive::<TestPermissions, _>(api::ArchiveTick);
    assert!(!waiting.is_finished());

    for (ii, req) in pending.into_iter().enumerate() {
        let response = req.await;
        log::info!("received response {ii}");
        assert_eq!(response.tick, tick.tick, "Archive counter should match tick after increment");
    }

    let tick = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;
    let response = waiting.await;
    assert_eq!(response.tick, tick.tick, "Archive counter should match tick after increment");
}

async fn test_archive_dropped_request(worker: &WorkerHandle) {
    let response = worker.async_archive::<TestPermissions, _>(api::ArchiveDrop);
    let result = response.await;
    assert!(result.is_none(), "Should return default value when server drops the request");
}

async fn test_fallible_subscriptions(worker: &WorkerHandle) {
    let scalar_success = worker
        .try_subscribe_scalar::<TestPermissions, _>(api::ScalarSubFallible { should_succeed: true })
        .await;
    match scalar_success {
        Ok(mut subscription) => {
            let _ = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;
            let _ = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;

            let first = subscription.next().await;
            let second = subscription.next().await;
            assert!(first.is_some(), "should receive first scalar event");
            assert!(second.is_some(), "should receive second scalar event");
        }
        Err(e) => {
            fail!("Expected successful scalar subscription but got error: {:?}", e);
        }
    }

    let scalar_fail = worker
        .try_subscribe_scalar::<TestPermissions, _>(api::ScalarSubFallible { should_succeed: false })
        .await;
    assert!(matches!(scalar_fail, Err(_)), "scalar subscription should fail");

    let archive_success = worker
        .try_subscribe_archive::<TestPermissions, _>(api::ArchiveSubFallible { should_succeed: true })
        .await;
    match archive_success {
        Ok(mut subscription) => {
            let _ = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;
            let _ = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;

            let first = subscription.next().await;
            let second = subscription.next().await;
            assert!(first.is_some(), "should receive first archive event");
            assert!(second.is_some(), "should receive second archive event");
        }
        Err(e) => {
            fail!("Expected successful archive subscription but got error: {:?}", e);
        }
    }

    let archive_fail = worker
        .try_subscribe_archive::<TestPermissions, _>(api::ArchiveSubFallible { should_succeed: false })
        .await;
    assert!(matches!(archive_fail, Err(_)), "archive subscription should fail");
}

async fn test_scalar_async(worker: &WorkerHandle) {
    let r1 = worker
        .async_scalar::<TestPermissions, _>(api::ScalarDoubleData(TestData { a: 1, b: 2, c: 3, d: false }))
        .await;
    let r2 = worker
        .async_scalar::<TestPermissions, _>(api::ScalarDoubleData(TestData { a: 10, b: 20, c: 30, d: true }))
        .await;

    assert_eq!(r1, TestData { a: 2, b: 4, c: 6, d: true });
    assert_eq!(r2, TestData { a: 20, b: 40, c: 60, d: false });
}

async fn test_scalar_async_dropped(worker: &WorkerHandle) {
    let response = worker.async_scalar::<TestPermissions, _>(test_server::api::ScalarDropRequest).await;
    assert_eq!(response, TestData::server_default());
}

async fn test_dropped_async_request_cancels_slot(worker: &WorkerHandle, control: &TestServerHandle) {
    assert_eq!(control.send_blocking_scalar(api::HeldScalarCount), 0, "held scalar queue should start empty");

    let dropped = worker.async_scalar::<TestPermissions, _>(api::HoldScalar);
    let held = worker.async_scalar::<TestPermissions, _>(api::HoldScalar);
    assert_eq!(
        worker.async_scalar::<TestPermissions, _>(api::HeldScalarCount).await,
        2,
        "held scalar requests should arrive"
    );

    drop(dropped);

    let released = control.send_blocking_scalar(api::ReleaseHeldScalars(2));
    assert_eq!(released, 2, "should release all held scalar requests");

    assert_eq!(held.await, 1);
    assert_eq!(control.send_blocking_scalar(api::HeldScalarCount), 0, "held scalar queue should end empty");
}

async fn test_try_async_with_dead_server(worker: &WorkerHandle) {
    let disposable = DisposableHandle::default();

    let echo_result =
        worker.async_scalar::<DisposablePermissions, _>(disposable_server::ScalarEcho(42)).await;
    assert_eq!(echo_result, 42, "disposable server echo");

    let pending =
        worker.try_async_scalar::<DisposablePermissions, _>(disposable_server::HoldDisposableScalar);
    std::thread::sleep(Duration::from_millis(50));

    log::info!("dropping disposable server");
    drop(disposable);

    assert_eq!(
        pending.await,
        Err(server::xous::Error::ProcessTerminated),
        "pending request should fail when the remote process disconnects"
    );
}

async fn test_worker_shutdown_on_drop(control: &TestServerHandle) {
    struct SetOnDrop(Arc<AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) { self.0.store(true, Ordering::Release); }
    }

    let worker = WorkerHandle::default();

    let task_started = Arc::new(AtomicBool::new(false));
    let task_dropped = Arc::new(AtomicBool::new(false));
    let sleep = worker.sleep(Duration::from_secs(60));
    worker
        .spawn({
            let task_started = Arc::clone(&task_started);
            let task_dropped = Arc::clone(&task_dropped);
            async move {
                let _set_on_drop = SetOnDrop(task_dropped);
                task_started.store(true, Ordering::Release);
                sleep.await;
            }
        })
        .detach();

    let response = worker.async_scalar::<TestPermissions, _>(api::HoldScalar);
    let _ = worker.async_scalar::<TestPermissions, _>(api::ScalarInterval).await;
    assert!(!response.is_closed(), "response should be open before drop");
    assert!(task_started.load(Ordering::Acquire), "detached task should start before drop");

    drop(worker);

    for _ in 0..20 {
        if response.is_closed() && task_dropped.load(Ordering::Acquire) {
            let _ = control.send_blocking_scalar(api::ReleaseHeldScalars(1));
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let _ = control.send_blocking_scalar(api::ReleaseHeldScalars(1));
    fail!("pending response stayed open or detached task stayed alive after dropping last handle");
}

async fn test_pending_connection_retry(worker: &WorkerHandle) {
    mod retry_server {
        use server::{listen_and_connect, xous, CheckedConn, Server};

        #[derive(server::Server)]
        #[name = "test/retry"]
        pub struct RetryServer;

        pub struct RetryServerHandle(CheckedConn<RetryPermissions>);

        impl Drop for RetryServerHandle {
            fn drop(&mut self) { self.0.try_send_scalar(ShutdownRetry).ok(); }
        }

        pub fn start_retry_server() -> RetryServerHandle {
            let server = RetryServer;
            let pid = xous::current_pid().expect("current pid");
            RetryServerHandle(listen_and_connect(server, pid).into())
        }

        #[derive(server::Message)]
        #[response(u32)]
        pub struct RetryEcho(pub u32);

        #[derive(server::Message)]
        struct ShutdownRetry;

        impl Server for RetryServer {}

        #[derive(Debug, Default, Clone, server::Permissions)]
        #[server_name = "test/retry"]
        #[all_permissions]
        pub struct RetryPermissions;

        impl server::BlockingScalarHandler<RetryEcho> for RetryServer {
            fn handle(&mut self, msg: RetryEcho, _: xous::PID, _: &mut server::ServerContext<Self>) -> u32 {
                msg.0
            }
        }

        impl server::ScalarHandler<ShutdownRetry> for RetryServer {
            fn handle(
                &mut self,
                _msg: ShutdownRetry,
                _: server::xous::PID,
                ctx: &mut server::ServerContext<Self>,
            ) {
                ctx.shutdown();
            }
        }
    }

    let _ = worker.async_archive::<TestPermissions, _>(api::ArchiveIncrementTick).await;
    assert!(!worker.get_retry_timer_active(), "should not have callback active");

    let pending_request =
        worker.async_scalar::<retry_server::RetryPermissions, _>(retry_server::RetryEcho(100));
    std::thread::sleep(Duration::from_millis(50));
    let immediate_request = worker
        .async_scalar::<TestPermissions, _>(api::ScalarDoubleData(TestData { a: 5, b: 10, c: 15, d: false }))
        .await;

    assert_eq!(
        immediate_request,
        TestData { a: 10, b: 20, c: 30, d: true },
        "worker can fulfill other requests while connection is blocked"
    );
    assert!(worker.get_retry_timer_active(), "should not have callback active yet");

    let _retry_server = retry_server::start_retry_server();
    let result = pending_request.await;

    assert!(!worker.get_retry_timer_active(), "callback should be cleared");
    assert_eq!(result, 100);
}

fn run_test<F>(test_name: &str, test: F)
where
    F: Future<Output = ()>,
{
    let start = std::time::Instant::now();
    match test_executor::block_timeout(test, Duration::from_secs(5)) {
        Some(_) => {
            let elapsed = start.elapsed();
            log::info!("✓ {} test passed (elapsed: {:?})", test_name, elapsed);
        }
        None => {
            fail!("test timed out {test_name}");
        }
    }
}

fn main() {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Debug);

    log::info!("Starting async worker integration tests...\n");

    let worker = WorkerHandle::default();

    run_test("basic task spawning", test_basic_task_spawning(&worker));
    run_test("sleep", test_worker_sleep(&worker));

    let test_server = test_server::start_test_server();

    run_test("scalar subscription", test_scalar_subscription(&worker));
    run_test("archive subscription", test_archive_subscription(&worker));
    run_test("subscription cancellation", test_subscription_cancellation(&worker));
    run_test("subscription drops old events", test_subscription_drops_old_events(&worker));
    run_test("fallible subscriptions", test_fallible_subscriptions(&worker));

    run_test("archive async", test_archive_async(&worker));
    run_test("archive dropped request", test_archive_dropped_request(&worker));
    run_test("scalar async", test_scalar_async(&worker));
    run_test("scalar async dropped", test_scalar_async_dropped(&worker));
    run_test(
        "dropped async request cancels slot",
        test_dropped_async_request_cancels_slot(&worker, &test_server),
    );
    run_test("pending connection retry", test_pending_connection_retry(&worker));

    run_test("worker shutdown on drop", test_worker_shutdown_on_drop(&test_server));
    run_test("try async with dead server", test_try_async_with_dead_server(&worker));

    run_test("timeout completes before timeout", test_timeout_completes_before_timeout(&worker));
    run_test("timeout expires", test_timeout_expires(&worker));

    log::info!("\nAll async worker integration tests passed successfully!");

    keyos_integration_test::pass();
}
