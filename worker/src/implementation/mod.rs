// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

pub use self::{
    cancel::{CancelOnDrop, CancelToken},
    slot_map::{RequestSlot, Slot, SubscriptionSlot},
    timer::Timer,
};

mod cancel;
mod slot_map;
mod timer;

use std::{
    cell::RefCell,
    collections::{binary_heap::PeekMut, BinaryHeap, LinkedList},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

use server::{xous, xous_names, MessageId as _, ServerContext};
use slot_map::{ReservedSlot, SlotKey, SlotMap};
use xous_ticktimer::TicktimerCallback;

const CONTROL_MESSAGES: usize = 16;
const CONTROL_START: usize = u32::MAX as usize - CONTROL_MESSAGES + 1;

thread_local! {
    static WORKER_SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
}

macro_rules! test_log {
    ($($arg:tt)*) => {
        #[cfg(feature = "integration_test")]
        log::debug!($($arg)*);
    };
}

#[derive(Debug)]
pub struct Inner {
    pub shared: Arc<Shared>,
    cancel_token: AtomicU64,
}

impl Drop for Inner {
    fn drop(&mut self) {
        server::try_send_scalar(self.shared.conn, Shutdown).ok();
        xous::disconnect(self.shared.conn).ok();
    }
}

impl Inner {
    #[inline]
    pub fn queue_event(&self, event: WorkerEvent) { self.shared.queue_event(event) }

    #[inline]
    pub fn next_cancel_token(self: &Arc<Self>) -> (CancelToken, CancelOnDrop) {
        let cancel_token = CancelToken(self.cancel_token.fetch_add(1, Ordering::Relaxed));
        (cancel_token, CancelOnDrop::new(Arc::downgrade(self), cancel_token))
    }
}

#[derive(Debug)]
pub struct Shared {
    pub conn: xous::CID,
    tx: mpsc::Sender<WorkerEvent>,
    wake_pending: AtomicBool,
}

impl Shared {
    #[inline]
    pub fn queue_event(&self, event: WorkerEvent) {
        if self.tx.send(event).is_ok() && !self.wake_pending.swap(true, Ordering::AcqRel) {
            if let Err(e) = server::try_send_scalar(self.conn, Wake) {
                log::error!("Could not wake: {e:?}");
            }
        }
    }
}

pub fn current_worker_shared() -> Option<Arc<Shared>> { WORKER_SHARED.with(|shared| shared.borrow().clone()) }

impl Default for Inner {
    fn default() -> Self {
        let pid = xous::current_pid().unwrap();
        let (tx, rx) = mpsc::channel();
        let sid = server::create_sid("");

        xous::allow_messages_on_server(sid, 0..CONTROL_START).unwrap();
        xous::register_server_event_handler(xous::ServerEvent::Disconnected, sid, ProcessDisconnected::ID)
            .unwrap();

        let conn = xous::connect_for_process(pid, sid).unwrap();
        let shared = Arc::new(Shared { conn, tx, wake_pending: AtomicBool::new(false) });

        let mut server = WorkerServer {
            sid,
            rx,
            shared: Arc::clone(&shared),
            names: xous_names::XousNames::new().unwrap(),
            connections: Default::default(),

            slots: SlotMap::new(),

            timers: BinaryHeap::new(),
            retry_cb_active: false,
            retry_queue: LinkedList::new(),
            ticktimer_cb: TicktimerCallback::new(sid).unwrap(),
        };
        std::thread::spawn(move || server.run());
        Self { shared, cancel_token: AtomicU64::new(CancelToken::START.0) }
    }
}

macro_rules! worker_msg {
    ($name:ident, $msg_id:expr) => {
        const _: () = assert!($msg_id < CONTROL_MESSAGES, "invalid message id");
        pub struct $name;
        impl server::AsScalar<1> for $name {
            fn as_scalar(&self) -> [u32; 1] { [0] }
        }
        impl server::FromScalar<1> for $name {
            fn from_scalar(_value: [u32; 1]) -> Self { Self }
        }
        impl server::MessageId for $name {
            const ID: xous::MessageId = CONTROL_START + $msg_id;
            const SERVER: &'static str = "";
        }
        impl server::Scalar for $name {}
    };
}

worker_msg!(Shutdown, 0);
worker_msg!(Wake, 1);
worker_msg!(RetryCallback, 2);
worker_msg!(TimerCallback, 3);
worker_msg!(ProcessDisconnected, 4);

pub struct WorkerServer {
    sid: xous::SID,
    shared: Arc<Shared>,
    names: xous_names::XousNames,

    rx: mpsc::Receiver<WorkerEvent>,
    connections: Vec<Cx>,

    slots: SlotMap,
    timers: BinaryHeap<Timer>,
    retry_queue: LinkedList<HandlerInit>,
    retry_cb_active: bool,
    ticktimer_cb: TicktimerCallback,
}

pub enum WorkerEvent {
    Task { task: async_task::Runnable },
    Register { init: HandlerInit },
    Timer { timer: Timer },
    Cancel { token: CancelToken },
}

pub struct HandlerInit {
    pub cancel_token: CancelToken,
    pub init: Box<dyn FnMut(&mut WorkerServer) -> Option<()> + Send>,
}

impl HandlerInit {
    fn run(&mut self, worker: &mut WorkerServer) -> Option<()> { (self.init)(worker) }
}

#[derive(Debug)]
struct Cx {
    server_name: &'static str,
    cid: xous::CID,
    pid: xous::PID,
}

impl Drop for Cx {
    fn drop(&mut self) { xous::disconnect(self.cid).ok(); }
}

impl WorkerServer {
    pub fn reserve_slot(&mut self) -> ReservedSlot<'_> { self.slots.reserve() }

    pub fn try_get_connection_from_name(
        &mut self,
        server_name: &'static str,
    ) -> Result<Option<(xous::CID, xous::CID, xous::PID)>, xous::Error> {
        let (cid, pid) = match self.connections.iter().find(|cx| cx.server_name == server_name) {
            Some(cx) => {
                log::debug!("found active connection for {server_name}");
                // re-retrieving the PID every time
                // to make sure the PID is still alive
                let pid = xous::get_remote_pid(cx.cid)?;
                (cx.cid, pid)
            }
            None => {
                log::debug!("no active connection for {server_name}, attempting to create new one");
                let Ok(cid) = self.names.request_connection(server_name) else { return Ok(None) };
                let pid = xous::get_remote_pid(cid)?;
                self.connections.push(Cx { server_name, cid, pid });
                (cid, pid)
            }
        };

        let cid_remote = xous::connect_for_process(pid, self.sid)?;

        Ok(Some((cid, cid_remote, pid)))
    }
}

impl WorkerServer {
    fn run(&mut self) {
        WORKER_SHARED.with(|shared| {
            *shared.borrow_mut() = Some(Arc::clone(&self.shared));
        });

        let mut context = ServerContext::<Self>::from_raw_sid(self.sid);
        while !context.shutdown {
            let msg = xous::receive_message(self.sid).unwrap();
            let cx = &mut context;
            let msg_id: xous::MessageId = msg.id();
            let pid = msg.sender.pid().unwrap();
            match msg_id {
                Shutdown::ID => server::handle_scalar_message::<Shutdown, _>(self, msg, cx),
                Wake::ID => server::handle_scalar_message::<Wake, _>(self, msg, cx),
                RetryCallback::ID => server::handle_scalar_message::<RetryCallback, _>(self, msg, cx),
                TimerCallback::ID => server::handle_scalar_message::<TimerCallback, _>(self, msg, cx),
                ProcessDisconnected::ID => {
                    server::handle_scalar_message::<ProcessDisconnected, _>(self, msg, cx)
                }
                #[cfg(feature = "integration_test")]
                GetRetryTimerActive::ID => {
                    server::handle_blocking_scalar_message::<GetRetryTimerActive, _>(self, msg, cx)
                }
                msg_id => {
                    let Some(key) = SlotKey::from_msg_id(msg_id) else {
                        log::warn!("received spurious message id {msg_id} from PID {pid}");
                        continue;
                    };
                    self.handle_dynamic_message(key, msg, pid);
                }
            };
        }
        xous::destroy_server(self.sid).unwrap();
        WORKER_SHARED.with(|shared| {
            *shared.borrow_mut() = None;
        });
    }

    fn handle_dynamic_message(&mut self, key: SlotKey, msg: xous::MessageEnvelope, pid: xous::PID) {
        let Some(slot) = self.slots.get_slot(key) else {
            return;
        };

        let slot_pid = slot.pid();
        if slot.pid() != pid {
            log::warn!("received message from invalid PID {pid}, expected {slot_pid}");
            return;
        }

        match (slot, key.is_cancel()) {
            (Slot::Subscription(handler), false) => {
                // latest events take precedence over older events
                let res = handler.tx.force_send(msg);
                test_log!("received sub event {res:?}");
                if let Ok(Some(_)) = res {
                    log::warn!("dropping subscription event due to full queue");
                }
            }
            (Slot::Subscription(_), true) => {
                test_log!("cancelled sub {}", key.msg_id());
                self.slots.free_key(key);
            }
            (Slot::Request(handler), false) => {
                if let Some(tx) = handler.tx.take() {
                    let _ = tx.send(Ok(msg));
                }
                test_log!("received response {}", key.msg_id());
                self.slots.free_key(key);
            }
            (Slot::Request(_), true) => {
                #[cfg(feature = "integration_test")]
                panic!("received cancel message for request slot");
            }
        }
    }

    fn cancel(&mut self, token: CancelToken) {
        if self.slots.remove_cancel_token(token) {
            test_log!("locally cancelled token {token:?}");
            return;
        }

        let mut cursor = self.retry_queue.cursor_front_mut();
        while let Some(init) = cursor.current() {
            if init.cancel_token == token {
                test_log!("cancelled in retry queue {token:?}");
                cursor.remove_current();
                break;
            } else {
                cursor.move_next();
            }
        }
    }

    fn request_retry_cb(&mut self) {
        if !self.retry_cb_active {
            self.request_callback(200, RetryCallback::ID);
            self.retry_cb_active = true;
        }
    }

    fn process_timers(&mut self) {
        let now = std::time::Instant::now();
        let duration = loop {
            let Some(timer) = self.timers.peek_mut() else {
                return;
            };
            let duration = timer.instant.saturating_duration_since(now);
            if duration == Duration::ZERO {
                let timer = PeekMut::pop(timer);
                timer.waker.wake();
            } else {
                break duration;
            }
        };

        test_log!("sleeping timer for {duration:?} {}", self.timers.len());
        // round up so the callback is never scheduled before the timer expires
        let millis = duration.as_nanos().div_ceil(1_000_000);
        self.request_callback(millis.try_into().unwrap_or(usize::MAX), TimerCallback::ID);
    }

    fn request_callback(&mut self, millis: usize, id: xous::MessageId) {
        self.ticktimer_cb.request(millis, id, 0);
    }
}

impl server::ServerMessages for WorkerServer {
    const NAME: &str = "";

    fn messages() -> &'static [server::MessageDef<Self>] { &[] }
}

impl server::Server for WorkerServer {}

impl server::ScalarHandler<Wake> for WorkerServer {
    fn handle(&mut self, _msg: Wake, _sender: xous::PID, _context: &mut server::ServerContext<Self>) {
        self.shared.wake_pending.store(false, Ordering::Release);

        let mut timer_dirty = false;

        while let Ok(task) = self.rx.try_recv() {
            match task {
                WorkerEvent::Task { task, .. } => {
                    task.run();
                }
                WorkerEvent::Register { mut init } => match init.run(self) {
                    None => {
                        self.retry_queue.push_back(init);
                        self.request_retry_cb();
                    }
                    Some(()) => {}
                },
                WorkerEvent::Timer { timer } => {
                    self.timers.push(timer);
                    timer_dirty = true;
                }
                WorkerEvent::Cancel { token } => self.cancel(token),
            }
        }

        if timer_dirty {
            self.process_timers();
        }
    }
}

impl server::ScalarHandler<Shutdown> for WorkerServer {
    fn handle(&mut self, _msg: Shutdown, _sender: xous::PID, context: &mut server::ServerContext<Self>) {
        log::debug!("shutting down worker");
        context.shutdown();
    }
}

impl server::ScalarHandler<RetryCallback> for WorkerServer {
    fn handle(&mut self, _msg: RetryCallback, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.retry_cb_active = false;

        let indices = 0..self.retry_queue.len();
        for _ in indices {
            let mut init = self.retry_queue.pop_front().unwrap();
            match init.run(self) {
                None => {
                    self.retry_queue.push_back(init);
                }
                Some(()) => {}
            }
        }

        if !self.retry_queue.is_empty() {
            self.request_retry_cb();
        }
    }
}

impl server::ScalarHandler<TimerCallback> for WorkerServer {
    fn handle(&mut self, _msg: TimerCallback, _sender: xous::PID, _context: &mut ServerContext<Self>) {
        self.process_timers();
    }
}

impl server::ScalarHandler<ProcessDisconnected> for WorkerServer {
    fn handle(&mut self, _msg: ProcessDisconnected, sender: xous::PID, _context: &mut ServerContext<Self>) {
        log::info!("process disconnected {sender}");
        self.connections.retain(|cx| cx.pid != sender);
        self.slots.disconnect_pid(sender);
    }
}

#[cfg(feature = "integration_test")]
mod test_msg {
    use super::*;

    worker_msg!(GetRetryTimerActive, 5);
    impl server::BlockingScalar for GetRetryTimerActive {
        type Response = bool;
    }
    impl server::BlockingScalarHandler<GetRetryTimerActive> for WorkerServer {
        fn handle(
            &mut self,
            _msg: GetRetryTimerActive,
            _sender: xous::PID,
            _context: &mut ServerContext<Self>,
        ) -> <GetRetryTimerActive as server::BlockingScalar>::Response {
            self.retry_cb_active
        }
    }
}

#[cfg(feature = "integration_test")]
pub use test_msg::*;
