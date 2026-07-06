// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

use core::mem;

use xous::{
    Error, MemoryAddress, MemoryMessage, MemoryRange, MemorySize, Message, MessageEnvelope, MessageId,
    MessageSender, ScalarMessage, ServerEvent, NUM_SERVER_EVENTS, PID, SID, TID,
};

#[cfg(keyos)]
use crate::mem::MemoryManager;
use crate::{
    mem::ClearShared,
    process::{current_pid, ThreadState},
    services::SystemServices,
};

/// Number of permission slots per connection in addition to the lower 0..63 message ids.
const MESSAGE_PERMISSION_COUNT: usize = 4;

/// A pointer to resolve a server ID to a particular process
#[derive(Debug)]
pub struct Server {
    /// A randomly-generated ID
    pub sid: SID,

    /// The process that owns this server
    pub pid: PID,

    /// Where messages should be inserted
    queue_head: usize,

    /// The index that the server is currently reading from
    queue_tail: usize,

    /// An increasing number that indicates where the server is reading.
    head_generation: u8,

    /// An increasing (but wrapping number) that indicates where clients are writing.
    tail_generation: u8,

    /// The number of empty queue slots
    empty_count: usize,

    /// Where data will appear
    #[cfg(keyos)]
    queue: &'static mut [QueuedMessage],

    #[cfg(not(keyos))]
    queue: Vec<QueuedMessage>,

    /// The `context mask` is a bitfield of contexts that are able to handle
    /// this message. If there are no available contexts, then messages will
    /// need to be queued.
    ready_threads: usize,

    pub default_permissions: MessagePermissions,

    event_handlers: [Option<MessageId>; NUM_SERVER_EVENTS],
}

pub struct SenderID {
    /// The index of the server within the SystemServices table
    pub sidx: usize,
    /// The index into the queue array
    pub idx: usize,
    /// The process ID that sent this message
    pid: Option<PID>,
}

impl SenderID {
    pub fn new(sidx: usize, idx: usize, pid: Option<PID>) -> Self { SenderID { sidx, idx, pid } }
}

impl From<usize> for SenderID {
    fn from(item: usize) -> SenderID {
        SenderID { sidx: (item >> 16) & 0xff, idx: item & 0xffff, pid: PID::new((item >> 24) as u8) }
    }
}

impl From<SenderID> for usize {
    fn from(val: SenderID) -> Self {
        (val.pid.map(|x| x.get() as usize).unwrap_or(0) << 24)
            | ((val.sidx << 16) & 0x00ff0000)
            | (val.idx & 0xffff)
    }
}

impl From<MessageSender> for SenderID {
    fn from(item: MessageSender) -> SenderID { SenderID::from(item.to_usize()) }
}

impl From<SenderID> for MessageSender {
    fn from(val: SenderID) -> Self { MessageSender::from_usize(val.into()) }
}

#[derive(Debug)]
pub enum WaitingMessage {
    /// There is no waiting message.
    None,

    /// The memory was borrowed and should be returned to the given process.
    BorrowedMemory { pid: PID, tid: TID, client_addr: MemoryAddress },

    /// The buffer answering a `BlockingMove` should be moved to the given process,
    /// unblocking it. `client_addr`/`buf_size` are the range it reserved on send, freed
    /// once the reply lands elsewhere.
    MovedMemory { pid: PID, tid: TID, client_addr: MemoryAddress, buf_size: MemorySize },

    /// The message was a scalar message, so you should return the result to the process
    ScalarMessage { pid: PID, tid: TID },

    /// The message was a scalar message, but the process that sent it no longer exists
    ScalarMessageTerminated,

    /// This memory should be returned to the system.
    ForgetMemory(MemoryRange),
}

/// Internal representation of a queued message for a server.
#[repr(usize)]
#[derive(PartialEq, Debug)]
enum QueuedMessage {
    Empty,
    BlockingScalarMessage {
        pid: PID,
        tid: u8,
        idx: u8,
        msg_id: usize,
        args: [usize; 4],
    },
    ScalarMessage {
        pid: PID,
        idx: u8,
        msg_id: usize,
        args: [usize; 4],
    },
    MemoryMessageSend {
        pid: PID,
        idx: u8,
        msg_id: usize,
        server_addr: MemoryAddress,
        buf_size: MemorySize,
        offset: usize,
        valid: usize,
    },
    /// A `Move` whose sender blocks until the server moves a buffer back. Keeps the
    /// sender's tid to unblock it, and `client_addr` to re-back its range if the server
    /// dies before responding.
    MemoryMessageBlockingSend {
        pid: PID,
        tid: u8,
        idx: u8,
        msg_id: usize,
        client_addr: MemoryAddress,
        server_addr: MemoryAddress,
        buf_size: MemorySize,
        offset: usize,
        valid: usize,
    },
    MemoryMessageROLend {
        pid: PID,
        tid: u8,
        idx: u8,
        msg_id: usize,
        client_addr: MemoryAddress,
        server_addr: MemoryAddress,
        buf_size: MemorySize,
        offset: usize,
        valid: usize,
    },
    MemoryMessageRWLend {
        pid: PID,
        tid: u8,
        idx: u8,
        msg_id: usize,
        client_addr: MemoryAddress,
        server_addr: MemoryAddress,
        buf_size: MemorySize,
        offset: usize,
        valid: usize,
    },
    /// The process lending this memory terminated before
    /// we could receive the message.
    MemoryMessageROLendTerminated {
        idx: u8,
        msg_id: usize,
        server_addr: MemoryAddress,
        buf_size: MemorySize,
        offset: usize,
        valid: usize,
    },

    /// The process lending this memory terminated before
    /// we could receive the message.
    MemoryMessageRWLendTerminated {
        idx: u8,
        msg_id: usize,
        server_addr: MemoryAddress,
        buf_size: MemorySize,
        offset: usize,
        valid: usize,
    },

    /// The sender of a `BlockingMove` terminated before we could receive it. The
    /// buffer it moved in is delivered anyway, but its eventual response is forgotten.
    MemoryMessageBlockingSendTerminated {
        idx: u8,
        msg_id: usize,
        server_addr: MemoryAddress,
        buf_size: MemorySize,
        offset: usize,
        valid: usize,
    },

    /// The process waiting for the response terminated before
    /// we could receive the message.
    BlockingScalarTerminated {
        idx: u8,
        msg_id: usize,
        args: [usize; 4],
    },

    /// When a message is taken that needs to be returned -- such as an ROLend
    /// or RWLend -- the slot is replaced with a WaitingReturnMemory token and its
    /// index is returned as the message sender.  This is used to unblock the
    /// sending process.
    WaitingReturnMemory {
        pid: PID,
        tid: u8,
        server_addr: MemoryAddress,
        client_addr: MemoryAddress,
        buf_size: MemorySize,
    },

    /// A delivered `BlockingMove` whose sender is parked until the server returns a
    /// buffer. Keeps the sender's original address and size so its range can be re-backed
    /// if the server dies first; the moved-in buffer itself is the server's now and is
    /// not tracked.
    WaitingReturnMoved {
        pid: PID,
        tid: u8,
        client_addr: MemoryAddress,
        buf_size: MemorySize,
    },

    /// A `WaitingReturnMoved` whose sender terminated; the returned buffer is forgotten
    /// rather than moved to a process that no longer exists.
    WaitingReturnMovedTerminated,

    /// When a server goes away, its memory must be forgotten instead of being returned
    /// to the previous process.
    WaitingForget {
        server_addr: MemoryAddress,
        buf_size: MemorySize,
    },

    /// This is the state when a message is blocking, but has no associated memory
    /// page.
    WaitingReturnScalar {
        pid: PID,
        tid: u8,
    },

    /// The process terminated while we were processing its blocking scalar
    WaitingReturnScalarTerminated,
}

// Size should be exactly 8 words / 32 bytes, yielding 128 queued messages per server
#[cfg(keyos)]
const _: () = assert!(core::mem::size_of::<QueuedMessage>() == 32);

#[derive(Debug, Clone, Default)]
pub struct MessagePermissions {
    mask: u64,
    list: [core::ops::Range<MessageId>; MESSAGE_PERMISSION_COUNT],
}

impl MessagePermissions {
    pub fn add(&mut self, messages: core::ops::Range<MessageId>) -> Result<xous::Result, Error> {
        if messages.is_empty() {
            return Err(Error::InvalidArguments);
        }
        for message_id in messages.start..(messages.end.min(64)) {
            self.mask |= 1 << message_id;
        }
        if messages.end <= 64 {
            return Ok(xous::Result::Ok);
        }
        for list_slot in &mut self.list {
            // If the slot and the requested range are contiguous, combine them.
            //
            // Illustration:
            // slot:  start<-------->end
            // msgs:         start<------->end
            //
            // slot:         start<------->end
            // msgs:  start<-------->end
            if list_slot.start <= messages.end && messages.start <= list_slot.end {
                *list_slot = list_slot.start.min(messages.start)..list_slot.end.max(messages.end);
                return Ok(xous::Result::Ok);
            }
            if (*list_slot).is_empty() {
                *list_slot = messages;
                return Ok(xous::Result::Ok);
            }
        }
        Err(Error::KernelTableFull)
    }

    pub fn is_permitted(&self, message_id: MessageId) -> bool {
        if message_id < 64 {
            self.mask & (1 << message_id) != 0
        } else {
            self.list.iter().any(|r| r.contains(&message_id))
        }
    }
}

impl Server {
    /// Initialize a server in the given option array. This function is
    /// designed to be called with `new` pointing to an entry in a vec.
    ///
    /// # Errors
    ///
    /// * **MemoryInUse**: The provided Server option already exists
    pub fn init(
        new: &mut Option<Server>,
        pid: PID,
        sid: SID,
        _backing: MemoryRange,
        initial_permissions: core::ops::Range<MessageId>,
    ) -> Result<(), Error> {
        if new.is_some() {
            return Err(Error::MemoryInUse);
        }

        #[cfg(keyos)]
        let queue = unsafe {
            core::slice::from_raw_parts_mut(
                _backing.as_mut_ptr() as *mut QueuedMessage,
                _backing.len() / mem::size_of::<QueuedMessage>(),
            )
        };

        #[cfg(not(keyos))]
        let queue = {
            let mut queue = vec![];
            // TODO: Replace this with a direct operation on a passed-in page
            queue.resize_with(crate::arch::mem::PAGE_SIZE / mem::size_of::<QueuedMessage>(), || {
                QueuedMessage::Empty
            });
            queue
        };
        let mut default_permissions = MessagePermissions::default();
        if !initial_permissions.is_empty() {
            default_permissions.add(initial_permissions)?;
        }

        *new = Some(Server {
            sid,
            pid,
            queue_head: 0,
            queue_tail: 0,
            head_generation: 0,
            tail_generation: 0,
            empty_count: queue.len(),
            queue,
            ready_threads: 0,
            default_permissions,
            event_handlers: [None; NUM_SERVER_EVENTS],
        });
        Ok(())
    }

    /// Take a current slot and replace it with `None`, clearing out the contents of the queue.
    pub fn destroy(mut self, ss: &mut SystemServices) {
        for entry in self.queue.iter_mut() {
            match *entry {
                // For `Empty` and `Scalar` messages, all we have to do is ignore them.
                // The sending process will not be blocked. These messages will be dropped,
                // and the server will never see them.
                // Same for processes that disappeared before we could service them
                QueuedMessage::Empty
                | QueuedMessage::ScalarMessage { .. }
                | QueuedMessage::BlockingScalarTerminated { .. }
                | QueuedMessage::WaitingReturnScalarTerminated
                | QueuedMessage::WaitingReturnMovedTerminated => {}

                // For `Send` messages, the Server has not yet seen these messages. Simply free it.
                // For lend and lendmut where the client disappeared, also just free the memory
                QueuedMessage::MemoryMessageSend { server_addr, buf_size, .. }
                | QueuedMessage::MemoryMessageBlockingSendTerminated { server_addr, buf_size, .. }
                | QueuedMessage::MemoryMessageROLendTerminated { server_addr, buf_size, .. }
                | QueuedMessage::MemoryMessageRWLendTerminated { server_addr, buf_size, .. } => {
                    #[cfg(keyos)]
                    MemoryManager::with_mut(|mm| mm.unmap_range(server_addr.get() as _, buf_size.get()))
                        .unwrap();
                    #[cfg(not(keyos))]
                    crate::arch::free_message_buffer(MemoryRange::from_parts(server_addr, buf_size));
                }

                QueuedMessage::WaitingForget { server_addr, buf_size } => {
                    #[cfg(keyos)]
                    MemoryManager::with_mut(|mm| mm.unmap_range(server_addr.get() as _, buf_size.get()))
                        .unwrap();
                    // The buffer was already freed; `server_addr` is a stale token.
                    #[cfg(not(keyos))]
                    let _ = (server_addr, buf_size);
                }

                // For messages where the client is waiting for a response, unblock the
                // client and return an error indicating the server does not exist.
                QueuedMessage::BlockingScalarMessage { pid, tid, .. }
                | QueuedMessage::WaitingReturnScalar { pid, tid, .. } => {
                    let tid = tid as _;

                    // Set the return value of the specified thread.
                    ss.set_thread_result(pid, tid, xous::Result::Error(Error::ServerNotFound)).unwrap();

                    // Mark it as ready to run.
                    ss.process_mut(pid).unwrap().set_thread_state(tid, ThreadState::Ready);
                }

                QueuedMessage::MemoryMessageROLend {
                    pid, tid, client_addr, server_addr, buf_size, ..
                }
                | QueuedMessage::MemoryMessageRWLend {
                    pid, tid, client_addr, server_addr, buf_size, ..
                }
                | QueuedMessage::WaitingReturnMemory {
                    pid, tid, client_addr, server_addr, buf_size, ..
                } => {
                    let client_pid = pid;
                    let client_tid = tid as _;
                    // Pages the borrower already freed can't be returned; re-back those as
                    // on-demand so the sender's range stays mappable.
                    if ss
                        .return_memory(
                            server_addr.get() as *mut usize,
                            client_pid,
                            client_tid,
                            client_addr.get() as _,
                            buf_size.get(),
                        )
                        .is_err()
                    {
                        ss.clear_shared_range(client_pid, client_addr, buf_size, ClearShared::OnDemand);
                    }
                    // An undelivered borrow's buffer is still ours to free; a delivered
                    // one was already freed at delivery.
                    #[cfg(not(keyos))]
                    if matches!(
                        *entry,
                        QueuedMessage::MemoryMessageROLend { .. } | QueuedMessage::MemoryMessageRWLend { .. }
                    ) {
                        crate::arch::free_message_buffer(MemoryRange::from_parts(server_addr, buf_size));
                    }
                    ss.process_mut(client_pid).unwrap().set_thread_state(client_tid, ThreadState::Ready);
                    ss.set_thread_result(client_pid, client_tid, xous::Result::Error(Error::ServerNotFound))
                        .unwrap();
                }

                // The server never took this buffer, so it is ours to free.
                QueuedMessage::MemoryMessageBlockingSend {
                    pid,
                    tid,
                    client_addr,
                    server_addr,
                    buf_size,
                    ..
                } => {
                    let tid = tid as _;
                    #[cfg(keyos)]
                    MemoryManager::with_mut(|mm| mm.unmap_range(server_addr.get() as _, buf_size.get()))
                        .unwrap();
                    #[cfg(not(keyos))]
                    crate::arch::free_message_buffer(MemoryRange::from_parts(server_addr, buf_size));
                    ss.clear_shared_range(pid, client_addr, buf_size, ClearShared::OnDemand);
                    ss.set_thread_result(pid, tid, xous::Result::Error(Error::ServerNotFound)).unwrap();
                    ss.process_mut(pid).unwrap().set_thread_state(tid, ThreadState::Ready);
                }

                // The buffer is the server's now and may already be freed or resized, so
                // leave it; only the sender's reserved range needs releasing to on-demand.
                QueuedMessage::WaitingReturnMoved { pid, tid, client_addr, buf_size } => {
                    let tid = tid as _;
                    ss.clear_shared_range(pid, client_addr, buf_size, ClearShared::OnDemand);
                    ss.set_thread_result(pid, tid, xous::Result::Error(Error::ServerNotFound)).unwrap();
                    ss.process_mut(pid).unwrap().set_thread_state(tid, ThreadState::Ready);
                }
            }
            *entry = QueuedMessage::Empty;
        }

        let server_pid = current_pid();

        // Finally, wake up all threads that are waiting on this Server.
        while let Some(server_tid) = self.take_available_thread() {
            ss.process_mut(server_pid).unwrap().set_thread_state(server_tid, ThreadState::Ready);
            ss.set_thread_result(server_pid, server_tid, xous::Result::Error(Error::ServerNotFound)).unwrap();
        }

        // Release the backing memory
        #[cfg(keyos)]
        MemoryManager::with_mut(|mm| {
            mm.unmap_range(self.queue.as_ptr() as _, core::mem::size_of_val(self.queue)).unwrap()
        });
    }

    pub fn is_queue_full(&self) -> bool { self.empty_count == 0 }

    /// When a process terminates, there may be memory that is lent to us.
    /// Mark all of that memory to be discarded when it is returned, rather than
    /// giving it back to the previous process space.
    pub fn discard_messages_for_pid(&mut self, pid: PID) {
        for entry in self.queue.iter_mut() {
            match *entry {
                QueuedMessage::MemoryMessageROLend {
                    pid: msg_pid,
                    idx,
                    msg_id,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                    ..
                } if msg_pid == pid => {
                    *entry = QueuedMessage::MemoryMessageROLendTerminated {
                        idx,
                        msg_id,
                        server_addr,
                        buf_size,
                        offset,
                        valid,
                    }
                }
                QueuedMessage::MemoryMessageRWLend {
                    pid: msg_pid,
                    idx,
                    msg_id,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                    ..
                } if msg_pid == pid => {
                    *entry = QueuedMessage::MemoryMessageRWLendTerminated {
                        idx,
                        msg_id,
                        server_addr,
                        buf_size,
                        offset,
                        valid,
                    }
                }
                QueuedMessage::MemoryMessageBlockingSend {
                    pid: msg_pid,
                    idx,
                    msg_id,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                    ..
                } if msg_pid == pid => {
                    *entry = QueuedMessage::MemoryMessageBlockingSendTerminated {
                        idx,
                        msg_id,
                        server_addr,
                        buf_size,
                        offset,
                        valid,
                    }
                }
                QueuedMessage::BlockingScalarMessage { pid: msg_pid, idx, msg_id, args, .. }
                    if msg_pid == pid =>
                {
                    *entry = QueuedMessage::BlockingScalarTerminated { idx, msg_id, args }
                }
                QueuedMessage::WaitingReturnMemory { pid: msg_pid, server_addr, buf_size, .. }
                    if msg_pid == pid =>
                {
                    *entry = QueuedMessage::WaitingForget { server_addr, buf_size }
                }
                QueuedMessage::WaitingReturnMoved { pid: msg_pid, .. } if msg_pid == pid => {
                    *entry = QueuedMessage::WaitingReturnMovedTerminated
                }
                QueuedMessage::WaitingReturnScalar { pid: msg_pid, .. } if msg_pid == pid => {
                    *entry = QueuedMessage::WaitingReturnScalarTerminated
                }

                // For "Scalar" and "Move" messages, this memory has already
                // been moved into this process, so memory will be reclaimed
                // when the process terminates.
                _ => (),
            }
        }
    }

    /// Convert a `QueuedMesage::WaitingReturnMemory` into `QueuedMessage::Empty`
    /// and return the pair.  Advance the tail.  Note that the `idx` could be
    /// somewhere other than the tail, but as long as it points to a valid
    /// message that's waiting a response, that's acceptable.
    pub fn take_waiting_message(
        &mut self,
        message_index: usize,
        buf: Option<&MemoryRange>,
    ) -> Result<WaitingMessage, Error> {
        #[cfg(not(keyos))]
        let _ = buf;
        let current_val = self.queue.get_mut(message_index).ok_or(Error::BadAddress)?;
        let result = match *current_val {
            QueuedMessage::WaitingReturnMemory { pid, tid, server_addr, client_addr, buf_size } => {
                if let Some(buf) = buf {
                    // The hosted address is a token into another process, so skip it there.
                    #[cfg(keyos)]
                    if server_addr.get() != buf.as_ptr() as usize {
                        return Err(Error::BadAddress);
                    }
                    if buf_size.get() != buf.len() {
                        return Err(Error::BadAddress);
                    }
                }
                #[cfg(not(keyos))]
                let _ = server_addr;

                WaitingMessage::BorrowedMemory { pid, tid: tid as _, client_addr }
            }
            QueuedMessage::WaitingForget { server_addr, buf_size } => {
                // Sanity check the specified address was correct
                #[cfg(keyos)]
                if let Some(buf) = buf {
                    if server_addr.get() != buf.as_ptr() as usize || buf_size.get() != buf.len() {
                        return Err(Error::BadAddress);
                    }
                }
                WaitingMessage::ForgetMemory(MemoryRange::from_parts(server_addr, buf_size))
            }
            QueuedMessage::WaitingReturnMoved { pid, tid, client_addr, buf_size } => {
                WaitingMessage::MovedMemory { pid, tid: tid as _, client_addr, buf_size }
            }
            QueuedMessage::WaitingReturnMovedTerminated => {
                // The sender is gone, so its returned buffer goes back to the system.
                WaitingMessage::ForgetMemory(*buf.ok_or(Error::BadAddress)?)
            }
            QueuedMessage::WaitingReturnScalar { pid, tid } => {
                WaitingMessage::ScalarMessage { pid, tid: tid as _ }
            }
            QueuedMessage::WaitingReturnScalarTerminated => WaitingMessage::ScalarMessageTerminated,
            _ => return Ok(WaitingMessage::None),
        };

        *current_val = QueuedMessage::Empty;
        self.empty_count += 1;
        self.queue_tail = message_index + 1;
        if self.queue_tail >= self.queue.len() {
            self.queue_tail = 0;
        }

        Ok(result)
    }

    /// Remove a message from the server's queue and replace it with either a
    /// QueuedMessage::WaitingReturnMemory or, for Scalar messages, QueuedMessage::Empty.
    ///
    /// For non-Scalar messages, you must call `take_waiting_message()` in order to return
    /// memory to the calling process.
    ///
    /// # Returns
    ///
    /// * **None**: There are no waiting messages ***Some(MessageEnvelope): This message is queued.
    pub fn take_next_message(&mut self, sidx: usize) -> Option<MessageEnvelope> {
        // If the reading head and tail generations are the same, the queue is empty.
        if self.tail_generation == self.head_generation {
            return None;
        }

        let mut queue_idx = self.queue_tail;
        loop {
            let (result, response) = match self.queue[queue_idx] {
                QueuedMessage::MemoryMessageROLend {
                    pid,
                    tid,
                    idx,
                    client_addr,
                    msg_id,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                } if idx == self.head_generation => (
                    MessageEnvelope {
                        sender: SenderID::new(sidx, queue_idx, Some(pid)).into(),
                        body: Message::Borrow(MemoryMessage {
                            id: msg_id,
                            buf: MemoryRange::from_parts(server_addr, buf_size),
                            offset: MemorySize::new(offset),
                            valid: MemorySize::new(valid),
                        }),
                    },
                    QueuedMessage::WaitingReturnMemory { pid, tid, server_addr, client_addr, buf_size },
                ),
                QueuedMessage::MemoryMessageRWLend {
                    pid,
                    tid,
                    idx,
                    client_addr,
                    msg_id,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                } if idx == self.head_generation => (
                    MessageEnvelope {
                        sender: SenderID::new(sidx, queue_idx, Some(pid)).into(),
                        body: Message::MutableBorrow(MemoryMessage {
                            id: msg_id,
                            buf: MemoryRange::from_parts(server_addr, buf_size),
                            offset: MemorySize::new(offset),
                            valid: MemorySize::new(valid),
                        }),
                    },
                    QueuedMessage::WaitingReturnMemory { pid, tid, server_addr, client_addr, buf_size },
                ),
                QueuedMessage::MemoryMessageROLendTerminated {
                    idx,
                    msg_id,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                } if idx == self.head_generation => (
                    MessageEnvelope {
                        sender: SenderID::new(sidx, queue_idx, PID::new(255)).into(),
                        body: Message::Borrow(MemoryMessage {
                            id: msg_id,
                            buf: MemoryRange::from_parts(server_addr, buf_size),
                            offset: MemorySize::new(offset),
                            valid: MemorySize::new(valid),
                        }),
                    },
                    QueuedMessage::WaitingForget { server_addr, buf_size },
                ),
                QueuedMessage::MemoryMessageRWLendTerminated {
                    idx,
                    msg_id,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                } if idx == self.head_generation => (
                    MessageEnvelope {
                        sender: SenderID::new(sidx, queue_idx, PID::new(255)).into(),
                        body: Message::MutableBorrow(MemoryMessage {
                            id: msg_id,
                            buf: MemoryRange::from_parts(server_addr, buf_size),
                            offset: MemorySize::new(offset),
                            valid: MemorySize::new(valid),
                        }),
                    },
                    QueuedMessage::WaitingForget { server_addr, buf_size },
                ),

                QueuedMessage::BlockingScalarMessage { pid, tid, idx, msg_id, args }
                    if idx == self.head_generation =>
                {
                    (
                        MessageEnvelope {
                            sender: SenderID::new(sidx, queue_idx, Some(pid)).into(),
                            body: Message::BlockingScalar(ScalarMessage {
                                id: msg_id,
                                arg1: args[0],
                                arg2: args[1],
                                arg3: args[2],
                                arg4: args[3],
                            }),
                        },
                        QueuedMessage::WaitingReturnScalar { pid, tid },
                    )
                }
                QueuedMessage::MemoryMessageSend {
                    pid,
                    idx,
                    msg_id,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                } if idx == self.head_generation => (
                    MessageEnvelope {
                        sender: SenderID::new(sidx, queue_idx, Some(pid)).into(),
                        body: Message::Move(MemoryMessage {
                            id: msg_id,
                            buf: MemoryRange::from_parts(server_addr, buf_size),
                            offset: MemorySize::new(offset),
                            valid: MemorySize::new(valid),
                        }),
                    },
                    QueuedMessage::Empty,
                ),
                QueuedMessage::MemoryMessageBlockingSend {
                    pid,
                    tid,
                    idx,
                    msg_id,
                    client_addr,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                } if idx == self.head_generation => (
                    MessageEnvelope {
                        sender: SenderID::new(sidx, queue_idx, Some(pid)).into(),
                        body: Message::BlockingMove(MemoryMessage {
                            id: msg_id,
                            buf: MemoryRange::from_parts(server_addr, buf_size),
                            offset: MemorySize::new(offset),
                            valid: MemorySize::new(valid),
                        }),
                    },
                    QueuedMessage::WaitingReturnMoved { pid, tid, client_addr, buf_size },
                ),
                QueuedMessage::MemoryMessageBlockingSendTerminated {
                    idx,
                    msg_id,
                    server_addr,
                    buf_size,
                    offset,
                    valid,
                } if idx == self.head_generation => (
                    MessageEnvelope {
                        sender: SenderID::new(sidx, queue_idx, PID::new(255)).into(),
                        body: Message::BlockingMove(MemoryMessage {
                            id: msg_id,
                            buf: MemoryRange::from_parts(server_addr, buf_size),
                            offset: MemorySize::new(offset),
                            valid: MemorySize::new(valid),
                        }),
                    },
                    QueuedMessage::WaitingReturnMovedTerminated,
                ),

                // Scalar messages have nothing to return, so they can go straight to the `Free` state
                QueuedMessage::ScalarMessage { pid, idx, msg_id, args } if idx == self.head_generation => (
                    MessageEnvelope {
                        sender: SenderID::new(sidx, queue_idx, Some(pid)).into(),
                        body: Message::Scalar(ScalarMessage {
                            id: msg_id,
                            arg1: args[0],
                            arg2: args[1],
                            arg3: args[2],
                            arg4: args[3],
                        }),
                    },
                    QueuedMessage::Empty,
                ),
                QueuedMessage::BlockingScalarTerminated { idx, msg_id, args }
                    if idx == self.head_generation =>
                {
                    (
                        MessageEnvelope {
                            sender: SenderID::new(sidx, queue_idx, PID::new(255)).into(),
                            body: Message::BlockingScalar(ScalarMessage {
                                id: msg_id,
                                arg1: args[0],
                                arg2: args[1],
                                arg3: args[2],
                                arg4: args[3],
                            }),
                        },
                        QueuedMessage::WaitingReturnScalarTerminated,
                    )
                }
                _ => {
                    queue_idx += 1;
                    if queue_idx >= self.queue.len() {
                        queue_idx = 0;
                    }
                    if queue_idx == self.queue_tail {
                        return None;
                    }
                    continue;
                }
            };

            self.queue_tail = queue_idx + 1;
            if self.queue_tail >= self.queue.len() {
                self.queue_tail = 0;
            }
            if matches!(response, QueuedMessage::Empty) {
                self.empty_count += 1;
            }
            self.queue[queue_idx] = response;
            self.head_generation = self.head_generation.wrapping_add(1);
            return Some(result);
        }
    }

    fn find_empty_slot(&mut self) -> core::result::Result<usize, Error> {
        for queue_idx in (self.queue_head..self.queue.len()).chain(0..self.queue_head) {
            if self.queue[queue_idx] == QueuedMessage::Empty {
                self.queue_head = queue_idx + 1;
                if self.queue_head >= self.queue.len() {
                    self.queue_head = 0;
                }
                return Ok(queue_idx);
            }
        }
        Err(Error::ServerQueueFull)
    }

    /// Add the given message to this server's queue.
    ///
    /// # Errors
    ///
    /// * **ServerQueueFull**: The server queue cannot accept any more messages
    pub fn queue_message(
        &mut self,
        pid: PID,
        tid: TID,
        message: Message,
        original_address: Option<MemoryAddress>,
    ) -> core::result::Result<usize, Error> {
        let queue_idx = self.find_empty_slot()?;
        let idx = self.tail_generation;
        self.queue[queue_idx] = match message {
            Message::Scalar(msg) => QueuedMessage::ScalarMessage {
                pid,
                idx,
                msg_id: msg.id,
                args: [msg.arg1, msg.arg2, msg.arg3, msg.arg4],
            },
            Message::BlockingScalar(msg) => QueuedMessage::BlockingScalarMessage {
                pid,
                tid: tid as _,
                idx,
                msg_id: msg.id,
                args: [msg.arg1, msg.arg2, msg.arg3, msg.arg4],
            },
            Message::Move(msg) => QueuedMessage::MemoryMessageSend {
                pid,
                idx,
                msg_id: msg.id,
                server_addr: MemoryAddress::new(msg.buf.as_ptr() as _).ok_or(Error::BadAddress)?,
                buf_size: MemorySize::new(msg.buf.len()).ok_or(Error::InvalidArguments)?,
                offset: msg.offset.map(|x| x.get()).unwrap_or(0),
                valid: msg.valid.map(|x| x.get()).unwrap_or(0),
            },
            Message::BlockingMove(msg) => QueuedMessage::MemoryMessageBlockingSend {
                pid,
                tid: tid as _,
                idx,
                msg_id: msg.id,
                client_addr: original_address.ok_or(Error::InvalidArguments)?,
                server_addr: MemoryAddress::new(msg.buf.as_ptr() as _).ok_or(Error::BadAddress)?,
                buf_size: MemorySize::new(msg.buf.len()).ok_or(Error::InvalidArguments)?,
                offset: msg.offset.map(|x| x.get()).unwrap_or(0),
                valid: msg.valid.map(|x| x.get()).unwrap_or(0),
            },
            Message::MutableBorrow(msg) => QueuedMessage::MemoryMessageRWLend {
                pid,
                tid: tid as _,
                idx,
                msg_id: msg.id,
                client_addr: original_address.ok_or(Error::InvalidArguments)?,
                server_addr: MemoryAddress::new(msg.buf.as_ptr() as _).ok_or(Error::BadAddress)?,
                buf_size: MemorySize::new(msg.buf.len()).ok_or(Error::InvalidArguments)?,
                offset: msg.offset.map(|x| x.get()).unwrap_or(0),
                valid: msg.valid.map(|x| x.get()).unwrap_or(0),
            },
            Message::Borrow(msg) => QueuedMessage::MemoryMessageROLend {
                pid,
                tid: tid as _,
                idx,
                msg_id: msg.id,
                client_addr: original_address.ok_or(Error::InvalidArguments)?,
                server_addr: MemoryAddress::new(msg.buf.as_ptr() as _).ok_or(Error::BadAddress)?,
                buf_size: MemorySize::new(msg.buf.len()).ok_or(Error::InvalidArguments)?,
                offset: msg.offset.map(|x| x.get()).unwrap_or(0),
                valid: msg.valid.map(|x| x.get()).unwrap_or(0),
            },
        };
        self.empty_count -= 1;

        // Advance the tail generation, which is used for incoming messages to keep
        // them in sequence.
        self.tail_generation = self.tail_generation.wrapping_add(1);
        assert_ne!(self.tail_generation, self.head_generation);

        Ok(queue_idx)
    }

    /// Directly queue the response to the message, because we are servicing it right now.
    pub fn queue_response(
        &mut self,
        pid: PID,
        tid: TID,
        message: &Message,
        client_address: Option<MemoryAddress>,
    ) -> core::result::Result<usize, Error> {
        let queue_idx = self.find_empty_slot()?;
        self.queue[queue_idx] = match message {
            Message::Scalar(_) | Message::BlockingScalar(_) => {
                QueuedMessage::WaitingReturnScalar { pid, tid: tid as _ }
            }
            Message::Move(msg) => QueuedMessage::WaitingForget {
                server_addr: MemoryAddress::new(msg.buf.as_ptr() as _).ok_or(Error::BadAddress)?,
                buf_size: MemorySize::new(msg.buf.len()).ok_or(Error::InvalidArguments)?,
            },
            Message::BlockingMove(msg) => QueuedMessage::WaitingReturnMoved {
                pid,
                tid: tid as _,
                client_addr: client_address.ok_or(Error::InvalidArguments)?,
                buf_size: MemorySize::new(msg.buf.len()).ok_or(Error::InvalidArguments)?,
            },
            Message::MutableBorrow(msg) | Message::Borrow(msg) => QueuedMessage::WaitingReturnMemory {
                pid,
                tid: tid as _,
                client_addr: client_address.ok_or(Error::InvalidArguments)?,
                server_addr: MemoryAddress::new(msg.buf.as_ptr() as _).ok_or(Error::BadAddress)?,
                buf_size: MemorySize::new(msg.buf.len()).ok_or(Error::InvalidArguments)?,
            },
        };
        self.empty_count -= 1;
        Ok(queue_idx)
    }

    /// Return a context ID that is available and blocking.  If no such context
    /// ID exists, or if this server isn't actually ready to receive packets,
    /// return None.
    pub fn take_available_thread(&mut self) -> Option<TID> {
        if self.ready_threads == 0 {
            return None;
        }
        let mut test_thread_mask = 1;
        let mut thread_number = 0;
        klog!("ready threads: 0b{:08b}", self.ready_threads);
        loop {
            // If the context mask matches this context number, remove it
            // and return the index.
            if self.ready_threads & test_thread_mask == test_thread_mask {
                self.ready_threads &= !test_thread_mask;
                return Some(thread_number);
            }
            // Advance to the next slot.
            test_thread_mask = test_thread_mask.rotate_left(1);
            thread_number += 1;
            if test_thread_mask == 1 {
                panic!("didn't find a free context, even though there should be one");
            }
        }
    }

    /// Return an available context to the blocking list.  This is part of the
    /// error condition when a message cannot be handled but the context has
    /// already been claimed.
    ///
    /// # Panics
    ///
    /// If the context cannot be returned because it is already blocking.
    pub fn return_available_thread(&mut self, tid: TID) {
        if self.ready_threads & (1 << tid) != 0 {
            panic!("tried to return context {}, but it was already blocking", tid);
        }
        self.ready_threads |= 1 << tid;
    }

    /// Add the given context to the list of ready and waiting contexts.
    pub fn park_thread(&mut self, tid: TID) {
        klog!("parking thread {}", tid);
        assert!(self.ready_threads & (1 << tid) == 0);
        self.ready_threads |= 1 << tid;
        klog!("ready threads now: {:08b}", self.ready_threads);
    }

    pub fn set_event_handler(&mut self, event: ServerEvent, id: MessageId) -> Result<(), Error> {
        if let Some(_existing) = &self.event_handlers[event as usize] {
            return Err(Error::MemoryInUse);
        }

        self.event_handlers[event as usize] = Some(id);

        Ok(())
    }

    pub fn get_event_handler(&self, event: ServerEvent) -> Option<MessageId> {
        self.event_handlers[event as usize].clone()
    }
}
