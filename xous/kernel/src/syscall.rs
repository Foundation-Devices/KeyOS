// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

use core::convert::TryInto;

use xous::{
    Error, MemoryAddress, MemoryFlags, MemoryMessage, MemoryRange, MemorySize, Message, MessageEnvelope,
    MessageSender, Result, SysCall, SysCallResult, ThreadPriority, CID, SID, TID,
};

use crate::irq::{interrupt_claim_user, interrupt_free};
use crate::mem::{ClearShared, MemoryManager, PAGE_SIZE};
use crate::process::{current_pid, ConnectionSlot, ThreadState, IRQ_TID};
use crate::scheduler::Scheduler;
use crate::server::{SenderID, WaitingMessage};
use crate::services::SystemServices;

#[derive(PartialEq)]
enum ExecutionType {
    Blocking,
    NonBlocking,
}

pub(crate) fn send_message(sender_tid: TID, cid: CID, message: Message) -> SysCallResult {
    SystemServices::with_mut(|ss| {
        // Frees the incoming buffer if we bail out below; cancelled once queued or
        // delivered, and a no-op for a message without memory.
        #[cfg(not(keyos))]
        let buf_guard = {
            let range = message.memory().copied();
            defer::defer(move || {
                if let Some(range) = range {
                    crate::arch::free_message_buffer(range);
                }
            })
        };

        let ConnectionSlot::Connected { sidx, permissions, .. } = ss.current_process().connection(cid)?
        else {
            return Err(Error::ServerNotFound);
        };
        let sidx = *sidx as usize;
        let server = ss.server_from_sidx(sidx).unwrap();
        if server.pid != current_pid() && !permissions.is_permitted(message.id()) {
            println!(
                "[!] Denying message send from {} to {:08x?}@{}, msgid={}",
                current_pid(),
                server.sid,
                server.pid,
                message.id(),
            );
            return Err(Error::AccessDenied);
        }
        let blocking = message.is_blocking();

        send_message_inner(ss, sender_tid, sidx, message)?;
        // Queued or delivered now, so the buffer is owned elsewhere; don't free it.
        #[cfg(not(keyos))]
        buf_guard.cancel();

        if blocking {
            ss.current_process_mut().set_thread_state(sender_tid, ThreadState::WaitBlocking { sidx });
        } else {
            ss.set_thread_result(current_pid(), sender_tid, Result::Ok)?;
        }
        // This may or may not change to the server, depending on process priorities.
        Scheduler::with_mut(|s| s.activate_current(ss))
    })
}

pub(crate) fn send_message_inner(
    ss: &mut SystemServices,
    sender_tid: TID,
    sidx: usize,
    message: Message,
) -> core::result::Result<(), Error> {
    let server = ss.server_from_sidx(sidx).expect("server couldn't be located");
    if server.is_queue_full() {
        return Err(Error::ServerQueueFull);
    }

    let server_pid = server.pid;
    let _sid = server.sid;

    // Remember the address the message came from, in case we need to
    // return it after the borrow is through.
    let client_address = match &message {
        Message::Scalar(_) | Message::BlockingScalar(_) => None,
        Message::Move(msg)
        | Message::MutableBorrow(msg)
        | Message::Borrow(msg)
        | Message::BlockingMove(msg) => MemoryAddress::new(msg.buf.as_ptr() as _),
    };

    // Translate memory messages from the client process to the server
    // process. Additionally, determine whether the call is blocking. If
    // so, switch to the server context right away.
    let message = match message {
        Message::Scalar(_) | Message::BlockingScalar(_) => message,
        Message::Move(msg) => {
            let new_virt = ss.send_memory(
                msg.buf.as_mut_ptr() as *mut usize,
                server_pid,
                core::ptr::null_mut(),
                msg.buf.len(),
            )?;
            Message::Move(MemoryMessage {
                id: msg.id,
                buf: unsafe { MemoryRange::new(new_virt as usize, msg.buf.len()) }?,
                offset: msg.offset,
                valid: msg.valid,
            })
        }
        Message::BlockingMove(msg) => {
            // Lend rather than move, so the sender's VA stays reserved (shared) while it
            // blocks; another thread mapping a freed range would corrupt the re-back.
            let new_virt = ss.lend_memory(
                msg.buf.as_mut_ptr() as *mut usize,
                server_pid,
                core::ptr::null_mut(),
                msg.buf.len(),
                true,
            )?;
            Message::BlockingMove(MemoryMessage {
                id: msg.id,
                buf: unsafe { MemoryRange::new(new_virt as usize, msg.buf.len()) }?,
                offset: msg.offset,
                valid: msg.valid,
            })
        }
        Message::MutableBorrow(msg) => {
            let new_virt = ss.lend_memory(
                msg.buf.as_mut_ptr() as *mut usize,
                server_pid,
                core::ptr::null_mut(),
                msg.buf.len(),
                true,
            )?;
            Message::MutableBorrow(MemoryMessage {
                id: msg.id,
                buf: unsafe { MemoryRange::new(new_virt as usize, msg.buf.len()) }?,
                offset: msg.offset,
                valid: msg.valid,
            })
        }
        Message::Borrow(msg) => {
            let new_virt = ss.lend_memory(
                msg.buf.as_mut_ptr() as *mut usize,
                server_pid,
                core::ptr::null_mut(),
                msg.buf.len(),
                false,
            )?;
            // println!(
            //     "Lending {} bytes from {:08x} in PID {} to {:08x} in PID {}",
            //     msg.buf.len(),
            //     msg.buf.as_mut_ptr() as usize,
            //     pid,
            //     new_virt as usize,
            //     server_pid,
            // );
            Message::Borrow(MemoryMessage {
                id: msg.id,
                buf: unsafe { MemoryRange::new(new_virt as usize, msg.buf.len()) }?,
                offset: msg.offset,
                valid: msg.valid,
            })
        }
    };

    let sender_pid = current_pid();
    // If the server has an available thread to receive the message,
    // transfer it right away.
    let server = ss.server_from_sidx_mut(sidx).expect("server couldn't be located");
    if let Some(server_tid) = server.take_available_thread() {
        // klog!(
        //     "there are threads available in PID {} to handle this message -- marking as Ready",
        //     server_pid
        // );
        let sender_idx = if message.is_blocking() {
            ss.remember_server_message(sidx, sender_pid, sender_tid, &message, client_address).inspect_err(
                |_e| {
                    klog!("error remembering server message: {:?}", _e);
                    ss.server_from_sidx_mut(sidx)
                        .expect("server couldn't be located")
                        .return_available_thread(server_tid);
                },
            )?
        } else {
            0
        };
        let sender = SenderID::new(sidx, sender_idx, Some(sender_pid));
        klog!("server connection data: sidx: {}, idx: {}, server pid: {}", sidx, sender_idx, server_pid);
        let envelope = MessageEnvelope { sender: sender.into(), body: message };

        ss.process_mut(server_pid).unwrap().set_thread_state(server_tid, ThreadState::Ready);
        // This only fails if the PID does not exist, in which case we can just drop the result.
        ss.set_thread_result(server_pid, server_tid, Result::MessageEnvelope(envelope)).ok();
    } else {
        klog!("no threads available in PID {} to handle this message, so queueing", server_pid);
        // Add this message to the queue.  If the queue is full, this returns an error.
        let _queue_idx = ss.queue_server_message(sidx, sender_pid, sender_tid, message, client_address)?;
        klog!("queued into index {:x}", _queue_idx);
    };
    Ok(())
}

fn return_memory(
    server_tid: TID,
    sender: MessageSender,
    buf: MemoryRange,
    offset: Option<MemorySize>,
    valid: Option<MemorySize>,
) -> SysCallResult {
    SystemServices::with_mut(|ss| {
        let sender = SenderID::from(sender);

        let server = ss.server_from_sidx_mut(sender.sidx).ok_or(Error::ServerNotFound)?;
        if server.pid != current_pid() {
            return Err(Error::ServerNotFound);
        }
        let queue_was_full = server.is_queue_full();
        let result = server.take_waiting_message(sender.idx, Some(&buf))?;
        if queue_was_full && !server.is_queue_full() {
            ss.wake_threads_with_state(ThreadState::RetryQueueFull { sidx: sender.sidx }, usize::MAX);
        }
        klog!("waiting message was: {:?}", result);
        match result {
            WaitingMessage::BorrowedMemory { pid, tid, client_addr } => {
                // Return the memory to the calling process
                ss.return_memory(buf.as_ptr() as _, pid, tid, client_addr.get() as _, buf.len())?;

                // On hardware the range is where the lent pages now live in the sender;
                // hosted ships this range on the wire, so it must point at our buffer.
                #[cfg(keyos)]
                let return_range = unsafe { MemoryRange::new(client_addr.get(), buf.len()) }?;
                #[cfg(not(keyos))]
                let return_range = buf;
                let return_value = Result::MemoryReturned(return_range, offset, valid);
                ss.process_mut(pid).unwrap().set_thread_state(tid, ThreadState::Ready);
                ss.set_thread_result(pid, tid, return_value)?;
            }
            WaitingMessage::MovedMemory { pid, tid, client_addr, buf_size } => {
                // The server owns `buf`; move it into the client at a fresh address.
                // (Hosted doesn't move pages -- `new_virt` is `buf`, shipped from the
                // range in `set_thread_result`.)
                let new_virt =
                    ss.send_memory(buf.as_mut_ptr() as *mut usize, pid, core::ptr::null_mut(), buf.len())?;
                let new_range = unsafe { MemoryRange::new(new_virt as usize, buf.len()) }?;
                // The reply landed elsewhere, so free the range the sender reserved on send.
                ss.clear_shared_range(pid, client_addr, buf_size, ClearShared::Free);

                let return_value = Result::MemoryReturned(new_range, offset, valid);
                ss.process_mut(pid).unwrap().set_thread_state(tid, ThreadState::Ready);
                ss.set_thread_result(pid, tid, return_value)?;
            }
            WaitingMessage::ForgetMemory(range) => {
                #[cfg(keyos)]
                MemoryManager::with_mut(|mm| mm.unmap_range(range.as_ptr(), range.len()))?;
                // The client is gone, so the returned bytes go nowhere; free the buffer.
                // (`range` is either stale or `buf` itself, so don't free it separately.)
                #[cfg(not(keyos))]
                {
                    let _ = range;
                    crate::arch::free_message_buffer(buf);
                }
            }
            WaitingMessage::ScalarMessage { .. } | WaitingMessage::ScalarMessageTerminated => {
                klog!("WARNING: Tried to wait on a message that was a scalar");
                return Err(Error::DoubleFree);
            }
            WaitingMessage::None => {
                klog!("WARNING: Tried to wait on a message that didn't exist -- return memory");
                return Err(Error::DoubleFree);
            }
        };

        ss.set_thread_result(current_pid(), server_tid, Result::Ok)?;
        Scheduler::with_mut(|s| s.activate_current(ss))
    })
}

fn return_result(server_tid: TID, sender: MessageSender, return_value: Result) -> SysCallResult {
    SystemServices::with_mut(|ss| {
        let sender = SenderID::from(sender);

        let server = ss.server_from_sidx_mut(sender.sidx).ok_or(Error::ServerNotFound)?;
        if server.pid != current_pid() {
            return Err(Error::ServerNotFound);
        }
        let queue_was_full = server.is_queue_full();
        let result = server.take_waiting_message(sender.idx, None)?;
        if queue_was_full && !server.is_queue_full() {
            ss.wake_threads_with_state(ThreadState::RetryQueueFull { sidx: sender.sidx }, usize::MAX);
        }
        match result {
            WaitingMessage::ScalarMessage { pid, tid } => {
                ss.process_mut(pid).unwrap().set_thread_state(tid, ThreadState::Ready);
                ss.set_thread_result(pid, tid, return_value)?;
            }
            WaitingMessage::ScalarMessageTerminated => {}
            WaitingMessage::ForgetMemory(_) => {
                klog!("WARNING: Tried to wait on a scalar message that was actually forgettingmemory");
                return Err(Error::DoubleFree);
            }
            WaitingMessage::BorrowedMemory { .. } | WaitingMessage::MovedMemory { .. } => {
                klog!("WARNING: Tried to wait on a scalar message that was actually memory");
                return Err(Error::DoubleFree);
            }
            WaitingMessage::None => {
                klog!(
                    "WARNING ({}:{}): Tried to wait on a message that didn't exist -- return {:?}",
                    current_pid().get(),
                    server_tid,
                    result
                );
                return Err(Error::DoubleFree);
            }
        };

        ss.set_thread_result(current_pid(), server_tid, Result::Ok)?;
        Scheduler::with_mut(|s| s.activate_current(ss))
    })
}

fn receive_message(tid: TID, sid: SID, blocking: ExecutionType) -> SysCallResult {
    SystemServices::with_mut(|ss| {
        // See if there is a pending message.  If so, return immediately.
        let sidx = ss.sidx_from_sid(sid, current_pid()).ok_or(Error::ServerNotFound)?;
        let server = ss.server_from_sidx_mut(sidx).ok_or(Error::ServerNotFound)?;
        // server.print_queue();

        // Ensure the server is for this PID
        if server.pid != current_pid() {
            return Err(Error::ServerNotFound);
        }

        let queue_was_full = server.is_queue_full();
        // If there is a pending message, return it immediately.
        if let Some(msg) = server.take_next_message(sidx) {
            klog!("waiting messages found -- returning {:x?}", msg);
            if queue_was_full && !server.is_queue_full() {
                ss.wake_threads_with_state(ThreadState::RetryQueueFull { sidx }, usize::MAX);
            }
            return Ok(Result::MessageEnvelope(msg));
        }

        if blocking == ExecutionType::NonBlocking {
            klog!("nonblocking message -- returning None");
            return Ok(Result::None);
        }
        // There is no pending message, so return control to the parent
        // process and mark ourselves as awaiting an event.  When a message
        // arrives, our return value will already be set to the
        // MessageEnvelope of the incoming message.
        klog!("did not have any waiting messages -- parking thread {}", tid);
        server.park_thread(tid);
        ss.current_process_mut().set_thread_state(tid, ThreadState::WaitReceive { sidx });
        Scheduler::with_mut(|s| s.activate_current(ss))
    })
}

fn check_syscall_permission(call: &SysCall) -> core::result::Result<(), Error> {
    let is_permitted_by_mask = || {
        let permission_mask = SystemServices::with(|ss| ss.current_process().syscall_permissions());
        if permission_mask & (1 << call.as_args()[0]) != 0 {
            Ok(())
        } else {
            println!("[!] PID {} called {call:?} without permission", current_pid());
            Err(Error::AccessDenied)
        }
    };
    match call {
        SysCall::Yield
        | SysCall::CreateThread(..)
        | SysCall::TerminateProcess(..)
        | SysCall::GetThreadId
        | SysCall::GetProcessId
        | SysCall::JoinThread(..)
        | SysCall::ServerEventHandler(..)
        | SysCall::SystemEventHandler(..)
        | SysCall::AppendPanicMessage(..)
        | SysCall::GetAppId(..)
        | SysCall::AppIdToPid(..) => Ok(()),

        // Messaging-related calls
        SysCall::CreateServer
        | SysCall::CreateServerId
        | SysCall::DestroyServer(..)
        | SysCall::Connect(..)
        | SysCall::TryConnect(..)
        | SysCall::ConnectForProcess(..)
        | SysCall::Disconnect(..)
        | SysCall::SendMessage(..)
        | SysCall::TrySendMessage(..)
        | SysCall::ReceiveMessage(..)
        | SysCall::TryReceiveMessage(..)
        | SysCall::GetRemoteProcessId(..)
        | SysCall::ReturnMemory(..)
        | SysCall::ReturnScalar1(..)
        | SysCall::ReturnScalar2(..)
        | SysCall::ReturnScalar5(..) => Ok(()),

        // XXX: Ideally this should be privileged, so that system servers with well-known SIDs can only be
        // registered by privileged processes (and the rest goes through the nameserver), but this is used in
        // various cases, like in the nameserver itself.
        SysCall::CreateServerWithAddress(..) => Ok(()),

        // Memory mapping has its own, more granular permission system
        SysCall::MapMemory(..) | SysCall::UnmapMemory(..) | SysCall::UpdateMemoryFlags(..) => Ok(()),

        SysCall::AllowMessagesSID(sid, _messages) => {
            // If the current process owns the SID then allow the operation
            if SystemServices::with(|ss| ss.sidx_from_sid(*sid, current_pid())).is_some() {
                Ok(())
            } else {
                is_permitted_by_mask()
            }
        }
        SysCall::AllowMessagesCID(pid, cid, _messages) => {
            let server_is_current_pid = SystemServices::with(|ss| {
                let ConnectionSlot::Connected { sidx, .. } = ss.process(*pid)?.connection(*cid)? else {
                    return Err(Error::ServerNotFound);
                };
                Ok(ss.server_from_sidx(*sidx as usize).ok_or(Error::ServerNotFound)?.pid == current_pid())
            })?;
            if server_is_current_pid {
                Ok(())
            } else {
                is_permitted_by_mask()
            }
        }

        #[cfg(keyos)]
        SysCall::FlushCache(..) | SysCall::FutexWait(..) | SysCall::FutexWake(..) => Ok(()),

        SysCall::SetThreadPriority(prio) if *prio < ThreadPriority::System0 => Ok(()),

        // Privileged calls
        SysCall::SetThreadPriority(..)
        | SysCall::ClaimInterrupt(..)
        | SysCall::FreeInterrupt(..)
        | SysCall::CreateProcess(..)
        | SysCall::Shutdown(..)
        | SysCall::PowerManagement(..)
        | SysCall::TerminatePid(..)
        | SysCall::MirrorMemoryToPid(..)
        | SysCall::GetSystemStats(..)
        | SysCall::GetPanicMessage(..) => is_permitted_by_mask(),

        #[cfg(keyos)]
        SysCall::VirtToPhys(..) | SysCall::VirtToPhysPid(..) | SysCall::DebugCommand(..) => {
            is_permitted_by_mask()
        }

        SysCall::Invalid(..) => Err(Error::UnhandledSyscall),
    }
}

pub fn handle(tid: TID, call: SysCall) -> SysCallResult {
    klog!("KERNEL({}:{}): Syscall {:x?}", crate::arch::process::current_pid(), tid, call);
    if tid == IRQ_TID && !call.can_call_from_interrupt() {
        klog!("[!] Called {:?} that's cannot be called from the interrupt handler!", call);
        return Err(Error::InvalidSyscall);
    };
    check_syscall_permission(&call)?;
    let result = match call {
        SysCall::MapMemory(phys, virt, size, req_flags) => {
            MemoryManager::with_mut(|mm| {
                let phys_ptr = phys.map(|x| x.get()).unwrap_or_default();
                let virt_ptr = virt.map(|x| x.get() as *mut usize).unwrap_or(core::ptr::null_mut());

                // Don't let the range exceed the user area (unless it's PID 1)
                if current_pid().get() != 1
                    && virt
                        .map(|x| x.get().checked_add(size.get()).is_none_or(|end| end > keyos::USER_AREA_END))
                        .unwrap_or(false)
                {
                    klog!("Exceeded user area");
                    return Err(Error::BadAddress);

                // Don't allow mapping non-page values
                } else if size.get() & (PAGE_SIZE - 1) != 0 {
                    // println!("map: bad alignment of size {:08x}", size);
                    return Err(Error::BadAlignment);
                }

                // Don't allow RWX pages
                if req_flags.is_set(MemoryFlags::W | MemoryFlags::X) {
                    klog!("Tried to map RWX page! phys=0x{phys:08x?}, virt=0x{virt:08x?}");
                    return Err(Error::InvalidArguments);
                }

                // A fresh populated mapping needs its pages zeroed; offload that to a DMA channel and park
                // the calling thread until it completes (see MemoryManager::map_with_deferred_zeroing). Only
                // an allocating request (phys == 0) on a parkable thread (not the IRQ handler) can defer;
                // everything else falls through to the synchronous path below.
                #[cfg(keyos)]
                if req_flags.is_set(MemoryFlags::POPULATE) && phys_ptr == 0 && tid != IRQ_TID {
                    return mm.map_with_deferred_zeroing(tid, virt_ptr, size.get(), req_flags);
                }

                let range = mm.map_range(phys_ptr, virt_ptr, size.get(), req_flags, true)?;

                Ok(Result::MemoryRange(range))
            })
        }
        SysCall::UnmapMemory(range) => {
            MemoryManager::with_mut(|mm| {
                mm.check_range_accessible(range)?;
                mm.unmap_range(range.as_ptr(), range.len())
            })?;
            SystemServices::with_mut(|ss| {
                ss.current_process_mut().release_allocation_hint(range.as_ptr() as usize)
            });
            Ok(Result::Ok)
        }
        SysCall::ClaimInterrupt(no, callback, arg) => {
            if let Ok(no) = no.try_into() {
                interrupt_claim_user(no, current_pid(), callback, arg).map(|_| Result::Ok)
            } else {
                Err(Error::InvalidArguments)
            }
        }
        SysCall::FreeInterrupt(no) => {
            if let Ok(no) = no.try_into() {
                interrupt_free(no, current_pid()).map(|_| Result::Ok)
            } else {
                Err(Error::InvalidArguments)
            }
        }
        #[cfg(keyos)]
        SysCall::FutexWait(addr, val) => MemoryManager::with(|mm| {
            use core::sync::atomic::{AtomicUsize, Ordering};

            if (addr & (core::mem::size_of::<usize>() - 1)) != 0 {
                return Err(Error::BadAlignment);
            }
            mm.check_range_accessible(unsafe { MemoryRange::new(addr, core::mem::size_of::<usize>())? })?;

            let got_val = unsafe { AtomicUsize::from_ptr(addr as *mut _).load(Ordering::SeqCst) };
            if val != got_val {
                return Err(Error::Again);
            }
            SystemServices::with_mut(|ss| {
                ss.set_thread_result(current_pid(), tid, xous::Result::Ok)?;
                ss.current_process_mut().set_thread_state(tid, ThreadState::WaitFutex { addr });
                Scheduler::with_mut(|s| s.activate_current(ss))
            })
        }),
        #[cfg(keyos)]
        SysCall::FutexWake(addr, n) => {
            SystemServices::with_mut(|ss| {
                ss.current_process_mut().wake_threads_with_state(ThreadState::WaitFutex { addr }, n)
            });
            Ok(Result::Ok)
        }
        SysCall::Yield => SystemServices::with_mut(|ss| {
            ss.set_thread_result(current_pid(), tid, Result::Ok)?;
            Scheduler::with_mut(|s| {
                let prio = ss.current_process().thread_priority(tid);
                s.yield_thread(current_pid(), tid, prio);
                s.activate_current(ss)
            })
        }),
        SysCall::SetThreadPriority(priority) => {
            if priority == ThreadPriority::Idle {
                Err(Error::InvalidArguments)
            } else {
                SystemServices::with_mut(|ss| {
                    ss.current_process_mut().set_thread_priority(tid, priority);
                    ss.set_thread_result(current_pid(), tid, Result::Ok)?;
                    Scheduler::with_mut(|s| s.activate_current(ss))
                })
            }
        }
        SysCall::ReceiveMessage(sid) => receive_message(tid, sid, ExecutionType::Blocking),
        SysCall::TryReceiveMessage(sid) => receive_message(tid, sid, ExecutionType::NonBlocking),
        SysCall::CreateThread(thread_init) => {
            SystemServices::with_mut(|ss| ss.create_thread(tid, thread_init).map(Result::ThreadID))
        }
        SysCall::CreateProcess(process_init) => {
            SystemServices::with_mut(|ss| ss.create_process(process_init).map(Result::NewProcess))
        }
        SysCall::CreateServerWithAddress(name, initial_range) => SystemServices::with_mut(|ss| {
            ss.create_server_with_address(name, initial_range).map(Result::NewServerID)
        }),
        SysCall::CreateServer => SystemServices::with_mut(|ss| {
            {
                let sid = ss.create_server_id()?;
                ss.create_server_with_address(sid, 0..0)
            }
            .map(Result::NewServerID)
        }),
        SysCall::CreateServerId => SystemServices::with_mut(|ss| ss.create_server_id().map(Result::ServerID)),
        SysCall::TryConnect(sid) => {
            SystemServices::with_mut(|ss| ss.connect_to_server(current_pid(), sid).map(Result::ConnectionID))
        }
        SysCall::ReturnMemory(sender, buf, offset, valid) => return_memory(tid, sender, buf, offset, valid),
        SysCall::ReturnScalar1(sender, arg) => return_result(tid, sender, Result::Scalar1(arg)),
        SysCall::ReturnScalar2(sender, arg1, arg2) => return_result(tid, sender, Result::Scalar2(arg1, arg2)),
        SysCall::ReturnScalar5(sender, arg1, arg2, arg3, arg4, arg5) => {
            return_result(tid, sender, Result::Scalar5(arg1, arg2, arg3, arg4, arg5))
        }
        SysCall::TrySendMessage(cid, message) => send_message(tid, cid, message),
        SysCall::TerminateProcess(ret) => SystemServices::with_mut(|ss| ss.terminate_current_process(ret)),
        SysCall::TerminatePid(pid, exit_code) => SystemServices::with_mut(|ss| {
            // The process is self-terminating, which is equivalent to TerminateProcess
            if pid == ss.current_process().pid {
                Err(Error::InvalidArguments)
            } else {
                ss.terminate_process(tid, pid, exit_code)
            }
        }),
        SysCall::Shutdown(_) => SystemServices::with_mut(|ss| ss.shutdown().map(|_| Result::Ok)),
        SysCall::GetProcessId => Ok(Result::ProcessID(current_pid())),
        SysCall::GetThreadId => Ok(Result::ThreadID(tid)),

        SysCall::Connect(sid) => {
            let result = SystemServices::with_mut(|ss| {
                ss.connect_to_server(current_pid(), sid).map(Result::ConnectionID)
            });
            match result {
                Ok(o) => Ok(o),
                Err(Error::ServerNotFound) => SystemServices::with_mut(|ss| {
                    ss.retry_syscall(tid, ThreadState::RetryConnect { sid_hash: sid.quick_hash() })
                }),
                Err(e) => Err(e),
            }
        }
        SysCall::ConnectForProcess(pid, sid) => {
            SystemServices::with_mut(|ss| ss.connect_to_server(pid, sid).map(Result::ConnectionID))
        }
        SysCall::SendMessage(cid, message) => {
            let result = send_message(tid, cid, message);
            match result {
                Ok(o) => Ok(o),
                Err(Error::ServerQueueFull) => SystemServices::with_mut(|ss| {
                    let ConnectionSlot::Connected { sidx, .. } = ss.current_process().connection(cid)? else {
                        return Err(Error::ServerNotFound);
                    };
                    let sidx = *sidx as usize;
                    ss.retry_syscall(tid, ThreadState::RetryQueueFull { sidx })
                }),
                Err(e) => Err(e),
            }
        }
        SysCall::Disconnect(cid) => {
            SystemServices::with_mut(|ss| ss.disconnect_from_server(cid).and(Ok(Result::Ok)))
        }
        SysCall::DestroyServer(sid) => {
            SystemServices::with_mut(|ss| ss.destroy_server(current_pid(), sid).and(Ok(Result::Ok)))
        }
        SysCall::JoinThread(other_tid) => SystemServices::with_mut(|ss| ss.join_thread(tid, other_tid)),
        SysCall::GetRemoteProcessId(cid) => SystemServices::with_mut(|ss| {
            let ConnectionSlot::Connected { sidx, .. } = ss.current_process().connection(cid)? else {
                return Err(Error::ServerNotFound);
            };
            let sidx = *sidx as usize;
            Ok(Result::ProcessID(ss.server_from_sidx(sidx).ok_or(Error::ServerNotFound)?.pid))
        }),
        SysCall::UpdateMemoryFlags(range, flags, pid) => {
            // We do not yet support modifying flags for other processes.
            if pid.is_some() {
                return Err(Error::ProcessNotChild);
            }

            MemoryManager::with_mut(|mm| mm.update_memory_flags(range, flags))?;
            Ok(Result::Ok)
        }
        #[cfg(keyos)]
        SysCall::VirtToPhys(vaddr) => crate::arch::mem::MemoryMapping::current()
            .virt_to_phys((vaddr & !(PAGE_SIZE - 1)) as *mut usize)
            .map(|pa| Result::Scalar1(pa | vaddr & (PAGE_SIZE - 1))),
        #[cfg(keyos)]
        SysCall::VirtToPhysPid(pid, vaddr) => {
            let pa = SystemServices::with_mut(|ss| {
                let Ok(proc) = ss.process(pid) else {
                    return Err(Error::ProcessNotFound);
                };
                proc.mapping.virt_to_phys((vaddr & !(PAGE_SIZE - 1)) as *mut usize)
            })?;
            Ok(Result::Scalar1(pa | vaddr & (PAGE_SIZE - 1)))
        }
        SysCall::GetAppId(pid) => match SystemServices::with(|ss| ss.process(pid).ok().map(|p| p.app_id())) {
            Some(app_id) => Ok(Result::Scalar5(
                u32::from_le_bytes(app_id.0[0..4].try_into().unwrap()) as usize,
                u32::from_le_bytes(app_id.0[4..8].try_into().unwrap()) as usize,
                u32::from_le_bytes(app_id.0[8..12].try_into().unwrap()) as usize,
                u32::from_le_bytes(app_id.0[12..16].try_into().unwrap()) as usize,
                1,
            )),
            None => Ok(Result::Scalar5(0, 0, 0, 0, 0)),
        },
        SysCall::AllowMessagesSID(sid, messages) => SystemServices::with_mut(|ss| {
            let Some(sidx) = ss.sidx_from_sid(sid, current_pid()) else {
                return Err(Error::ServerNotFound);
            };
            ss.server_from_sidx_mut(sidx).unwrap().default_permissions.add(messages)
        }),
        SysCall::AllowMessagesCID(pid, cid, messages) => {
            if cid < 2 {
                return Err(Error::ServerNotFound);
            }
            SystemServices::with_mut(|ss| match ss.process_mut(pid)?.connection_mut(cid) {
                Ok(ConnectionSlot::Connected { permissions, .. }) => permissions.add(messages),
                _ => Err(Error::ServerNotFound),
            })
        }
        #[cfg(keyos)]
        SysCall::FlushCache(mem, op) => MemoryManager::with(|mm| {
            mm.check_range_accessible(mem)?;
            crate::arch::mem::MemoryMapping::current().flush_cache(mem, op)?;
            Ok(Result::Ok)
        }),
        #[cfg(keyos)]
        SysCall::PowerManagement(dram) => {
            crate::platform::set_dram_idle_mode(dram);
            Ok(Result::Ok)
        }
        SysCall::AppIdToPid(app_id) => {
            let pid = SystemServices::with(|ss| ss.pid_from_app_id(app_id));
            if let Some(pid) = pid {
                return Ok(Result::ProcessID(pid));
            }

            Ok(Result::None)
        }
        #[cfg(keyos)]
        SysCall::MirrorMemoryToPid(mem, pid) => {
            let source_mapping = crate::arch::mem::MemoryMapping::current();
            let mem_phys = source_mapping.virt_to_phys(mem.as_ptr() as *const usize)?;

            // Check that the process owns the memory range both virtually and physically continuous
            for (i, page) in
                (mem.as_ptr() as usize..(mem.as_ptr() as usize + mem.len())).step_by(PAGE_SIZE).enumerate()
            {
                let page_phys = source_mapping.virt_to_phys(page as *const usize)?;
                let page_phys_expected = mem_phys + i * PAGE_SIZE;
                klog!(
                    "mirror_memory_to_pid: checking page {:08x}: got {:08x}, expected {:08x}",
                    page,
                    page_phys,
                    page_phys_expected
                );

                if page_phys != page_phys_expected {
                    klog!("mirror_memory_to_pid: physical range is not continuous");
                    return Err(Error::AccessDenied);
                }
            }

            let mirror_range = MemoryManager::with_mut(|mm| {
                SystemServices::with_mut(|ss| {
                    ss.process(pid)?.mapping.activate();
                    let res = mm.map_range_readonly_mirror(pid, mem_phys, mem.len());
                    source_mapping.activate();
                    res
                })
            })?;

            Ok(Result::MemoryRange(mirror_range))
        }
        #[cfg(keyos)]
        SysCall::DebugCommand(mut buffer, cmd) => MemoryManager::with_mut(|mm| {
            mm.check_range_accessible(buffer)?;
            let start = (buffer.as_ptr() as usize) & !(PAGE_SIZE - 1);
            let end = ((buffer.as_ptr() as usize) + buffer.len()).next_multiple_of(PAGE_SIZE);
            for addr in (start..end).step_by(PAGE_SIZE) {
                mm.ensure_backed(addr as _)?;
            }
            let mut buffer = crate::debug::BufStr::from(&mut buffer);
            crate::debug::commands::debug_command(cmd, &mut buffer);
            Ok(Result::Scalar1(buffer.as_slice().len()))
        }),
        SysCall::GetSystemStats(stat) => match stat {
            xous::SystemStat::FreeMemory => Ok(Result::Scalar1(MemoryManager::with(|mm| mm.ram_free()))),
            xous::SystemStat::IsSystemLowOnMemory => {
                Ok(Result::Scalar1(MemoryManager::with(|mm| mm.low_memory()) as usize))
            }
        },
        SysCall::ServerEventHandler(event, sid, id) => SystemServices::with_mut(|ss| {
            let sidx = ss.sidx_from_sid(sid, current_pid()).ok_or(Error::ServerNotFound)?;
            let server = ss.server_from_sidx_mut(sidx).ok_or(Error::ServerNotFound)?;
            server.set_event_handler(event, id).and(Ok(Result::Ok))
        }),
        SysCall::SystemEventHandler(event, sid, id) => SystemServices::with_mut(|ss| {
            ss.current_process_mut().set_system_event_handler(event, sid, id).and(Ok(Result::Ok))
        }),

        SysCall::AppendPanicMessage(len, a1, a2, a3, a4, a5, a6) => {
            let mut buf = [0u8; size_of::<usize>() * 6];
            let len = len.min(buf.len());

            let mut num_bytes = 0;
            for word in [a1, a2, a3, a4, a5, a6].iter() {
                for byte in word.to_le_bytes() {
                    buf[num_bytes] = byte;
                    num_bytes += 1;
                    if num_bytes >= len {
                        break;
                    }
                }
            }

            SystemServices::with_mut(|ss| ss.append_panic_message(&buf[..len]).and(Ok(Result::Ok)))
        }

        SysCall::GetPanicMessage(buf) => MemoryManager::with(|mm| {
            mm.check_range_accessible(buf)?;

            SystemServices::with_mut(|ss| {
                let (pid, msg) = ss.get_panic_message();
                let pid_val = pid.map(|p| p.get() as usize).unwrap_or(0);

                let copy_len = msg.len().min(buf.len());
                if copy_len > 0 {
                    let user_buf = unsafe { core::slice::from_raw_parts_mut(buf.as_mut_ptr(), copy_len) };
                    user_buf.copy_from_slice(&msg[..copy_len]);
                }

                Ok(Result::Scalar2(pid_val, copy_len))
            })
        }),

        _ => Err(Error::UnhandledSyscall),
    };
    klog!(
        " -> ({}:{}) {:x?}",
        crate::arch::process::current_pid(),
        crate::arch::process::Process::current().current_tid(),
        result
    );
    result
}
