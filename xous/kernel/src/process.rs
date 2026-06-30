// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

use keyos::{MEMORY_MIRROR_AREA_VIRT, MMAP_AREA_VIRT, MMAP_AREA_VIRT_END, PAGE_SIZE};
#[cfg(keyos)]
use xous::arch::MAX_PROCESS_NAME_LEN;
use xous::{AppId, Error, MessageId, SystemEvent, ThreadPriority, CID, NUM_SYSTEM_EVENTS, PID, SID, TID};

use crate::arch::mem::MemoryMapping;
pub use crate::arch::process::Process as ArchProcess;
pub use crate::arch::process::{current_pid, MAX_THREAD_COUNT};
use crate::scheduler::Scheduler;
use crate::server::MessagePermissions;
#[cfg(keyos)]
const MEMORY_PERMISSION_COUNT: usize = 8;

pub const MAX_CONNECTIONS: usize = 64;

/// Maximum size of the panic message buffer
pub const PANIC_MESSAGE_SIZE: usize = 1024;

pub const INITIAL_TID: TID = 1;
pub const IRQ_TID: TID = 0;

pub struct Process {
    /// The absolute MMU address.  If 0, then this process is free.  This needs
    /// to be available so we can switch to this process at any time, so it
    /// cannot go into the "inner" struct.
    pub mapping: MemoryMapping,

    /// This process' PID. This should match up with the index in the process table.
    pub pid: PID,

    /// The process that created this process, which tells who is allowed to
    /// manipulate this process.
    pub ppid: Option<PID>,

    /// Descriptive name
    #[cfg(keyos)]
    name: Option<[u8; MAX_PROCESS_NAME_LEN]>,

    /// The states of the individual threads
    threads: [ThreadState; MAX_THREAD_COUNT],

    /// Priorities of individual threads
    thread_priorities: [ThreadPriority; MAX_THREAD_COUNT],

    event_handlers: [Option<EventHandler>; NUM_SYSTEM_EVENTS],

    /// Unique App identifier (different from `name`)
    app_id: AppId,

    /// Special permissions the process has
    permissions: ProcessPermissions,

    /// A mapping of connection IDs to server indexes
    connection_map: [ConnectionSlot; MAX_CONNECTIONS],

    /// The virtual address of the last allocation, as a hint
    pub allocation_hint: usize,

    /// The virtual address to use for the next mirror allocation
    pub next_mirror_address: usize,

    /// ASLR slide applied when loading the ELF
    /// This is only used to make sense of a backtrace after a crash
    #[cfg(keyos)]
    pub(crate) aslr_slide: usize,
}

#[derive(Debug, Default)]
struct ProcessPermissions {
    #[cfg(keyos)]
    memory: [core::ops::Range<usize>; MEMORY_PERMISSION_COUNT],
    syscall: u64,
}

#[derive(Debug, Clone, Default)]
pub enum ConnectionSlot {
    #[default]
    Free,
    Tombstone {
        refcount: usize,
    },
    Connected {
        sidx: u8,
        refcount: usize,
        permissions: MessagePermissions,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    /// Unallocated
    Free,
    /// Either running or ready to run immediately
    Ready,
    /// Waiting on join_thread()
    WaitJoin { tid: usize },
    /// Waiting on a blocking message send() to return
    WaitBlocking { sidx: usize },
    /// Waiting on a receive()
    WaitReceive { sidx: usize },
    /// Waiting on futex_wait()
    #[allow(dead_code)]
    WaitFutex { addr: usize },
    /// Retrying a connect() call because the server does not exist (yet). PC is on the SWI instruction, so
    /// once it's marked ready, the connect() syscall will be executed again.
    RetryConnect { sid_hash: u32 },
    /// Retrying a send() call because the server's queue was full. PC is on the SWI instruction, so once
    /// it's marked ready, the connect() syscall will be executed again.
    RetryQueueFull { sidx: usize },
    /// Owner of an in-flight map_memory() DMA page-zeroing. The pages are allocated but deliberately not yet
    /// mapped. The thread is parked past the SWI; the zeroing-completion interrupt maps the pages, delivers
    /// the resulting range (or error) as the syscall result, and marks the thread ready.
    #[allow(dead_code)]
    WaitMapZero,
    /// Waiting for the single map_memory() zeroing channel to become free. Nothing is allocated yet. PC is
    /// on the SWI instruction, so the syscall re-runs once a turn opens up.
    #[allow(dead_code)]
    RetryMapZero,
}

#[derive(Debug, Clone)]
pub struct EventHandler {
    pub sid: SID,
    pub message_id: MessageId,
}

impl Process {
    pub fn new(mapping: MemoryMapping, pid: PID, ppid: PID, app_id: AppId) -> Process {
        Process {
            mapping,
            pid,
            ppid: Some(ppid),
            #[cfg(keyos)]
            name: None,
            threads: [ThreadState::Free; MAX_THREAD_COUNT],
            event_handlers: [const { None }; NUM_SYSTEM_EVENTS],
            thread_priorities: [ThreadPriority::AppDefault; MAX_THREAD_COUNT],
            app_id,
            permissions: Default::default(),
            connection_map: [const { ConnectionSlot::Free }; MAX_CONNECTIONS],
            allocation_hint: MMAP_AREA_VIRT,
            next_mirror_address: MEMORY_MIRROR_AREA_VIRT,
            #[cfg(keyos)]
            aslr_slide: 0,
        }
    }

    pub fn activate(&self) {
        crate::arch::process::set_current_pid(self.pid);
        self.mapping.activate();
    }

    /// Lower the allocation hint to `virt` when a MMAP-area range is freed, so
    /// the next allocation reuses it instead of marching ever forward.
    pub fn release_allocation_hint(&mut self, virt: usize) {
        if (MMAP_AREA_VIRT..MMAP_AREA_VIRT_END).contains(&virt) {
            self.allocation_hint = self.allocation_hint.min(virt);
        }
    }

    /// Find a virtual address in this process big enough for `size` bytes, advancing the
    /// process's allocation hint past the returned region. A non-null `virt_ptr` is returned as-is.
    pub fn find_virtual_address(&mut self, virt_ptr: *mut usize, size: usize) -> Result<*mut usize, Error> {
        if !virt_ptr.is_null() {
            return Ok(virt_ptr);
        }

        if size > MMAP_AREA_VIRT_END - MMAP_AREA_VIRT {
            return Err(Error::OutOfMemory);
        }

        let needed = size / PAGE_SIZE;

        // Search forward from the hint, then wrap to the area below it.
        let start = self
            .mapping
            .find_free_run(self.allocation_hint, MMAP_AREA_VIRT_END, needed)
            .or_else(|| {
                self.mapping.find_free_run(
                    MMAP_AREA_VIRT,
                    self.allocation_hint.saturating_add(size).min(MMAP_AREA_VIRT_END),
                    needed,
                )
            })
            .ok_or(Error::BadAddress)?;

        self.allocation_hint = (start + size).min(MMAP_AREA_VIRT_END);
        Ok(start as *mut usize)
    }

    pub fn terminate(&mut self, _ret: u32) -> Result<(), Error> {
        #[cfg(keyos)]
        println!("[*] PID {} (`{}`) exited with code {}", self.pid, self.name().unwrap_or("N/A"), _ret);

        #[cfg(feature = "trace-systemview")]
        {
            systemview_keyos::SystemView::task_exec_end();
        }

        for tid in 1..MAX_THREAD_COUNT {
            self.set_thread_state(tid, ThreadState::Free);
        }

        // Free all associated memory pages
        unsafe {
            crate::mem::MemoryManager::with_mut(|mm| mm.release_all_memory_for_process(&mut self.mapping))
        };

        // Free all claimed IRQs
        crate::irq::release_interrupts_for_pid(self.pid);

        // Remove this PID from the process table
        ArchProcess::destroy(self.pid)?;
        self.mapping.destroy();

        Ok(())
    }

    pub fn thread_state(&self, tid: TID) -> ThreadState { self.threads[tid] }

    pub fn set_thread_state(&mut self, tid: TID, state: ThreadState) {
        let prio = self.thread_priority(tid);
        if self.threads[tid] == ThreadState::Ready && state != ThreadState::Ready {
            Scheduler::with_mut(|s| s.park_thread(self.pid, tid, prio));
        }
        if self.threads[tid] != ThreadState::Ready && state == ThreadState::Ready {
            Scheduler::with_mut(|s| s.ready_thread(self.pid, tid, prio));
        }
        self.threads[tid] = state;
    }

    #[allow(dead_code)]
    pub fn set_thread_priority(&mut self, tid: TID, priority: ThreadPriority) {
        let current_priority = self.thread_priority(tid);
        if current_priority == priority {
            return;
        }
        if self.threads[tid] == ThreadState::Ready {
            Scheduler::with_mut(|s| s.park_thread(self.pid, tid, current_priority));
            Scheduler::with_mut(|s| s.ready_thread(self.pid, tid, priority));
        }
        self.thread_priorities[tid] = priority
    }

    #[allow(dead_code)]
    pub fn thread_priority(&self, tid: TID) -> ThreadPriority { self.thread_priorities[tid] }

    /// Returns the process name, if any, of a given PID
    #[cfg(keyos)]
    pub fn name(&self) -> Option<&str> {
        // Check the new process names table
        let name_bytes = self.name.as_ref()?;
        let name_len = name_bytes.iter().position(|b| *b == 0).unwrap_or(MAX_PROCESS_NAME_LEN);
        let name = core::str::from_utf8(&name_bytes[..name_len]).ok()?;
        if !name.is_empty() {
            Some(name)
        } else {
            None
        }
    }

    #[cfg(keyos)]
    pub fn set_name(&mut self, name_bytes: &[u8]) -> Result<(), Error> {
        if name_bytes.len() > MAX_PROCESS_NAME_LEN {
            println!(
                "[!] The name for the new process PID {} is too long: {} (max {})",
                self.pid,
                name_bytes.len(),
                MAX_PROCESS_NAME_LEN
            );
            return Err(Error::InvalidString);
        }

        if let Some(_curr_name) = self.name() {
            println!(
                "[!] The name is already set for the PID {}. Current name is: `{}`",
                self.pid, _curr_name
            );

            // Name is already set for this process
            return Err(Error::InternalError);
        }

        let mut name_buf = [0u8; MAX_PROCESS_NAME_LEN];
        name_buf[..name_bytes.len()].copy_from_slice(name_bytes);
        self.name = Some(name_buf);
        Ok(())
    }

    pub fn app_id(&self) -> AppId { self.app_id }

    #[cfg(keyos)]
    pub fn check_memory_permission(&self, addr: usize) -> Result<(), Error> {
        if self.pid.get() == 1
            || keyos::is_address_in_plaintext_dram(addr)
            || keyos::is_address_encrypted(addr)
        {
            return Ok(());
        }

        for additional_region in &self.permissions.memory {
            if additional_region.contains(&addr) {
                return Ok(());
            }
            if additional_region.end == 0 {
                break;
            }
        }
        Err(Error::AccessDenied)
    }

    #[cfg(keyos)]
    pub fn add_memory_permission(&mut self, addr_range: core::ops::Range<usize>) -> Result<(), Error> {
        for additional_region in &mut self.permissions.memory {
            // Find a free slot and put the new permission there
            if additional_region.end == 0 {
                *additional_region = addr_range;
                return Ok(());
            }
        }
        Err(Error::KernelTableFull)
    }

    pub fn syscall_permissions(&self) -> u64 { self.permissions.syscall }

    pub fn set_syscall_permissions(&mut self, permission_mask: u64) {
        self.permissions.syscall = permission_mask;
    }

    pub fn set_system_event_handler(
        &mut self,
        event: SystemEvent,
        sid: SID,
        id: MessageId,
    ) -> Result<(), Error> {
        klog!("Registering system event {event:?} handler for SID {:?}, PID = {}", sid, self.pid);

        if let Some(_existing) = &self.event_handlers[event as usize] {
            klog!("Handler already registered for SID {:?}", _existing.sid);
            return Err(Error::MemoryInUse);
        }

        self.event_handlers[event as usize] = Some(EventHandler { sid, message_id: id });

        Ok(())
    }

    pub fn get_event_handler(&self, event: SystemEvent) -> Option<(SID, MessageId)> {
        self.event_handlers[event as usize].as_ref().map(|e| (e.sid, e.message_id))
    }

    pub fn wake_threads_with_state(&mut self, state: ThreadState, mut n: usize) {
        if n == 0 {
            return;
        }
        for tid in 1..MAX_THREAD_COUNT {
            if self.thread_state(tid) == state {
                self.set_thread_state(tid, ThreadState::Ready);
                n -= 1;
                if n == 0 {
                    return;
                }
            }
        }
    }

    pub fn tombstone_connection_by_sidx(&mut self, dead_sidx: usize) -> Option<CID> {
        for (cidx, connection_slot) in self.connection_map.iter_mut().enumerate() {
            match connection_slot {
                ConnectionSlot::Connected { sidx, refcount, .. } if *sidx == dead_sidx as u8 => {
                    *connection_slot = ConnectionSlot::Tombstone { refcount: *refcount };
                    return Some(cidx as CID + 2);
                }
                _ => (),
            }
        }
        None
    }

    /// Returns CID, true if a new connection was made,
    /// Returns CID, false if a connection already existed
    pub fn add_connection(
        &mut self,
        sidx: usize,
        permissions: MessagePermissions,
    ) -> Result<(CID, bool), Error> {
        for (cidx, connection) in self.connection_map.iter_mut().enumerate() {
            match connection {
                ConnectionSlot::Connected { sidx: sidx_other, refcount, .. } if *sidx_other == sidx as u8 => {
                    *refcount += 1;
                    return Ok((cidx as CID + 2, false));
                }
                _ => {}
            }
        }
        if let Some(cidx) = self.connection_map.iter().position(|c| matches!(c, ConnectionSlot::Free)) {
            self.connection_map[cidx] =
                ConnectionSlot::Connected { sidx: sidx as u8, permissions, refcount: 1 };
            Ok(((cidx as CID) + 2, true))
        } else {
            Err(Error::KernelTableFull)
        }
    }

    pub fn connection(&self, cid: CID) -> Result<&ConnectionSlot, Error> {
        if cid < 2 {
            return Err(Error::ServerNotFound);
        }
        self.connection_map.get(cid as usize - 2).ok_or(Error::ServerNotFound)
    }

    pub fn connection_mut(&mut self, cid: CID) -> Result<&mut ConnectionSlot, Error> {
        if cid < 2 {
            return Err(Error::ServerNotFound);
        }
        self.connection_map.get_mut(cid as usize - 2).ok_or(Error::ServerNotFound)
    }

    #[allow(dead_code)]
    pub fn number_of_connections(&self) -> usize {
        self.connection_map.iter().filter(|c| !matches!(c, ConnectionSlot::Free)).count()
    }

    pub fn connected_sidxes(&self) -> impl Iterator<Item = usize> {
        self.connection_map.clone().into_iter().filter_map(|c| {
            if let ConnectionSlot::Connected { sidx, .. } = c {
                Some(sidx as usize)
            } else {
                None
            }
        })
    }
}

impl core::fmt::Debug for Process {
    fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::result::Result<(), core::fmt::Error> {
        write!(
            fmt,
            "Process {} (threads={})",
            self.pid.get(),
            self.threads.iter().filter(|t| **t != ThreadState::Free).count(),
        )
    }
}
