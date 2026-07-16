// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::ops::Deref;

use server::{
    listen_and_connect, wrapped_scalar, xous, ArchiveEventSubscriber, ArchiveRequest, BlockingScalarRequest,
    CheckedConn, ScalarEventSubscriber, Server, ServerContext,
};

pub struct TestServerHandle(CheckedConn<TestPermissions>);

impl Deref for TestServerHandle {
    type Target = CheckedConn<TestPermissions>;

    fn deref(&self) -> &Self::Target { &self.0 }
}

impl Drop for TestServerHandle {
    fn drop(&mut self) {
        log::info!("dropping test server handle");
        self.0.try_send_scalar(Shutdown).ok();
    }
}

pub fn start_test_server() -> TestServerHandle {
    let server = TestServer::default();
    let pid = xous::current_pid().expect("current pid");
    TestServerHandle(listen_and_connect(server, pid).into())
}

use api::*;
pub mod api {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TestData {
        pub a: u8,
        pub b: u16,
        pub c: u32,
        pub d: bool,
    }

    impl TestData {
        pub fn server_default() -> Self { TestData { a: 99, b: 999, c: 9999, d: true } }
    }

    impl server::AsScalar<4> for TestData {
        fn as_scalar(&self) -> [u32; 4] { [self.a as u32, self.b as u32, self.c, self.d as u32] }
    }

    impl server::FromScalar<4> for TestData {
        fn from_scalar(value: [u32; 4]) -> Self {
            TestData { a: value[0] as u8, b: value[1] as u16, c: value[2], d: value[3] != 0 }
        }
    }

    #[derive(server::Message)]
    #[response(TestData)]
    pub struct ScalarDoubleData(pub TestData);

    #[derive(server::Message)]
    #[response(TestData)]
    pub struct ScalarDropRequest;

    #[derive(Debug, rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    pub enum TestSubscriptionError {
        InvalidTickNumber,
    }

    #[derive(server::Message, rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    #[response(TickResponse)]
    pub struct ArchiveIncrementTick;

    #[derive(rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    pub struct TickResponse {
        pub tick: u8,
    }

    #[derive(server::Message, rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    #[event(ScalarTickEvent)]
    pub struct ScalarTickSub;

    pub struct ScalarTickEvent(pub u8);
    wrapped_scalar!(ScalarTickEvent);

    #[derive(server::Message, rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    #[event(ArchiveTickEvent)]
    pub struct ArchiveTickSub;

    #[derive(rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    pub struct ArchiveTickEvent(pub u8);
    #[derive(server::Message, rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    #[response(DropResponse)]
    pub struct ArchiveDropAllSubs;

    #[derive(rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    pub struct DropResponse {
        pub dropped_count: usize,
    }

    #[derive(server::Message)]
    pub struct Shutdown;

    #[derive(server::Message, rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    #[response(TickResponse)]
    pub struct ArchiveTick;

    // Archive message that drops the request without responding
    #[derive(server::Message, rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    #[response(Option<TickResponse>)]
    pub struct ArchiveDrop;

    #[derive(server::Message, rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    #[event(ScalarTickEvent)]
    #[error(TestSubscriptionError)]
    pub struct ScalarSubFallible {
        pub should_succeed: bool,
    }

    #[derive(server::Message, rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
    #[event(ArchiveTickEvent)]
    #[error(TestSubscriptionError)]
    pub struct ArchiveSubFallible {
        pub should_succeed: bool,
    }

    #[derive(server::Message)]
    #[response(usize)]
    pub struct ScalarInterval;

    #[derive(server::Message)]
    #[response(usize)]
    pub struct HoldScalar;

    #[derive(server::Message)]
    #[response(usize)]
    pub struct HeldScalarCount;

    #[derive(server::Message)]
    #[response(usize)]
    pub struct ReleaseHeldScalars(pub usize);
}

#[derive(Debug, Default, Clone, server::Permissions)]
#[all_permissions]
#[server_name = "test/worker-server"]
pub struct TestPermissions;

#[derive(Default, server::Server)]
#[name = "test/worker-server"]
pub struct TestServer {
    tick_number: u8,
    scalar_subscriptions: Vec<ScalarEventSubscriber<ScalarTickEvent>>,
    archive_subscriptions: Vec<ArchiveEventSubscriber<ArchiveTickEvent>>,

    archive_requests: Vec<ArchiveRequest<ArchiveTick>>,
    held_scalar_requests: Vec<BlockingScalarRequest<HoldScalar>>,
}

impl TestServer {
    fn process_tick(&mut self) -> u8 {
        // Increment tick number first
        self.tick_number += 1;

        // Handle pending archive async requests with new tick number
        self.archive_requests.drain(..).for_each(|request| {
            request.response.respond(TickResponse { tick: self.tick_number }).unwrap();
        });

        // Send events with new tick number
        let event = ScalarTickEvent(self.tick_number);
        for subscription in &self.scalar_subscriptions {
            let _ = subscription.send(&event);
        }

        let event = ArchiveTickEvent(self.tick_number);
        for subscription in &self.archive_subscriptions {
            let _ = subscription.send(&event);
        }

        self.tick_number
    }
}

impl Server for TestServer {}

impl server::BlockingArchiveHandler<ArchiveIncrementTick> for TestServer {
    fn handle(
        &mut self,
        _msg: ArchiveIncrementTick,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> TickResponse {
        let new_tick = self.process_tick();
        TickResponse { tick: new_tick }
    }
}

impl server::ScalarEventSubscriptionHandler<ScalarTickSub> for TestServer {
    fn handle(
        &mut self,
        _msg: ScalarTickSub,
        subscriber: ScalarEventSubscriber<ScalarTickEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.scalar_subscriptions.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<ArchiveTickSub> for TestServer {
    fn handle(
        &mut self,
        _msg: ArchiveTickSub,
        subscriber: ArchiveEventSubscriber<ArchiveTickEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        self.archive_subscriptions.push(subscriber);
        Ok(())
    }
}

impl server::BlockingArchiveHandler<ArchiveDropAllSubs> for TestServer {
    fn handle(
        &mut self,
        _msg: ArchiveDropAllSubs,
        _pid: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> DropResponse {
        log::info!("Dropping all subscribers");
        let scalar_count = self.scalar_subscriptions.len();
        let archive_count = self.archive_subscriptions.len();
        self.scalar_subscriptions.clear();
        self.archive_subscriptions.clear();
        DropResponse { dropped_count: scalar_count + archive_count }
    }
}

impl server::ScalarHandler<Shutdown> for TestServer {
    fn handle(
        &mut self,
        _msg: Shutdown,
        _sender: server::xous::PID,
        context: &mut server::ServerContext<Self>,
    ) {
        log::info!("shutting down test server");
        context.shutdown();
    }
}

// Archive test messages

impl server::BlockingArchiveAsyncHandler<ArchiveTick> for TestServer {
    fn handle(&mut self, request: ArchiveRequest<ArchiveTick>, _context: &mut server::ServerContext<Self>) {
        self.archive_requests.push(request);
    }

    fn default_response() -> TickResponse { TickResponse { tick: 0 } }
}

impl server::BlockingArchiveAsyncHandler<ArchiveDrop> for TestServer {
    fn handle(&mut self, _request: ArchiveRequest<ArchiveDrop>, _context: &mut server::ServerContext<Self>) {
        // Intentionally drop the request without responding
        log::info!("Dropping Archive request without responding");
    }

    fn default_response() -> Option<TickResponse> { None }
}

impl server::BlockingScalarHandler<ScalarDoubleData> for TestServer {
    fn handle(
        &mut self,
        ScalarDoubleData(data): ScalarDoubleData,
        _sender: xous::PID,
        _context: &mut server::ServerContext<Self>,
    ) -> TestData {
        TestData {
            a: data.a.saturating_mul(2),
            b: data.b.saturating_mul(2),
            c: data.c.saturating_mul(2),
            d: !data.d,
        }
    }
}

impl server::BlockingScalarAsyncHandler<ScalarInterval> for TestServer {
    fn handle(&mut self, request: BlockingScalarRequest<ScalarInterval>, _context: &mut ServerContext<Self>) {
        let _ = request.response.respond(1);
    }

    fn default_response() -> <ScalarInterval as server::BlockingScalar>::Response { 0 }
}

impl server::BlockingScalarAsyncHandler<HoldScalar> for TestServer {
    fn handle(&mut self, request: BlockingScalarRequest<HoldScalar>, _context: &mut ServerContext<Self>) {
        self.held_scalar_requests.push(request);
    }

    fn default_response() -> <HoldScalar as server::BlockingScalar>::Response { 0 }
}

impl server::BlockingScalarHandler<HeldScalarCount> for TestServer {
    fn handle(
        &mut self,
        _msg: HeldScalarCount,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> usize {
        self.held_scalar_requests.len()
    }
}

impl server::BlockingScalarHandler<ReleaseHeldScalars> for TestServer {
    fn handle(
        &mut self,
        ReleaseHeldScalars(count): ReleaseHeldScalars,
        _sender: xous::PID,
        _context: &mut ServerContext<Self>,
    ) -> usize {
        let count = count.min(self.held_scalar_requests.len());
        for request in self.held_scalar_requests.drain(..count) {
            let _ = request.response.respond(1);
        }
        count
    }
}

// Handler that drops request
impl server::BlockingScalarAsyncHandler<ScalarDropRequest> for TestServer {
    fn handle(
        &mut self,
        _request: BlockingScalarRequest<ScalarDropRequest>,
        _context: &mut server::ServerContext<Self>,
    ) {
        log::info!("dropping scalar request without responding")
    }

    fn default_response() -> TestData { TestData::server_default() }
}

impl server::ScalarEventSubscriptionHandler<ScalarSubFallible> for TestServer {
    fn handle(
        &mut self,
        msg: ScalarSubFallible,
        subscriber: ScalarEventSubscriber<ScalarTickEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), TestSubscriptionError> {
        if !msg.should_succeed {
            return Err(TestSubscriptionError::InvalidTickNumber);
        }

        self.scalar_subscriptions.push(subscriber);
        Ok(())
    }
}

impl server::ArchiveEventSubscriptionHandler<ArchiveSubFallible> for TestServer {
    fn handle(
        &mut self,
        msg: ArchiveSubFallible,
        subscriber: ArchiveEventSubscriber<ArchiveTickEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), TestSubscriptionError> {
        if !msg.should_succeed {
            return Err(TestSubscriptionError::InvalidTickNumber);
        }

        self.archive_subscriptions.push(subscriber);
        Ok(())
    }
}
