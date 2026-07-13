// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

#[cfg(keyos)]
use core::ptr::{addr_of, addr_of_mut};

#[cfg(keyos)]
use xous::arch::MAX_PROCESS_NAME_LEN;
use xous::ServerEvent;
use xous::{
    arch::ProcessStartup, pid_from_usize, AppId, Error, MemoryAddress, MemoryRange, MemorySize, Message,
    ProcessInit, SystemEvent, ThreadInit, CID, PID, SID, TID,
};

use crate::arch::mem::MemoryMapping;
pub use crate::arch::process::Process as ArchProcess;
pub use crate::arch::process::MAX_PROCESS_COUNT;
pub use crate::arch::process::MAX_THREAD_COUNT;
use crate::debug::BufStr;
use crate::mem::ClearShared;
use crate::platform;
use crate::process::{current_pid, ConnectionSlot, Process, ThreadState, INITIAL_TID, PANIC_MESSAGE_SIZE};
use crate::scheduler::Scheduler;
use crate::server::Server;

const MAX_SERVER_COUNT: usize = 128;

/// A big unifying struct containing all of the system state.
pub struct SystemServices {
    /// A table of all processes in the system
    pub processes: [Option<Process>; MAX_PROCESS_COUNT],

    /// A table of all servers in the system
    pub servers: [Option<Server>; MAX_SERVER_COUNT],

    /// Panic message buffer, shared across all processes and only one can panic at a time
    panic_message: BufStr<[u8; PANIC_MESSAGE_SIZE]>,

    /// PID of the process that "owns" the current panic message
    panic_message_pid: Option<PID>,

    /// In-flight permission requests parked on the permission broker
    pub permission_requests: crate::permission_requests::PermissionRequests,
}

#[cfg(not(keyos))]
std::thread_local!(static SYSTEM_SERVICES: core::cell::RefCell<SystemServices> = core::cell::RefCell::new(SystemServices {
    processes: [const { None }; MAX_PROCESS_COUNT],
    servers: [const { None }; 128],
    panic_message: BufStr::new(),
    panic_message_pid: None,
    permission_requests: crate::permission_requests::PermissionRequests::new(),
}));

#[cfg(keyos)]
#[no_mangle]
static mut SYSTEM_SERVICES: SystemServices = SystemServices {
    processes: [const { None }; MAX_PROCESS_COUNT],
    servers: [const { None }; MAX_SERVER_COUNT],
    panic_message: BufStr::new(),
    panic_message_pid: None,
    permission_requests: crate::permission_requests::PermissionRequests::new(),
};

impl SystemServices {
    /// Calls the provided function with the current inner process state.
    #[allow(dead_code)]
    pub fn with<F, R>(f: F) -> R
    where
        F: FnOnce(&SystemServices) -> R,
    {
        #[cfg(keyos)]
        unsafe {
            f(&*addr_of!(SYSTEM_SERVICES))
        }
        #[cfg(not(keyos))]
        SYSTEM_SERVICES.with(|ss| f(&ss.borrow()))
    }

    pub fn with_mut<F, R>(f: F) -> R
    where
        F: FnOnce(&mut SystemServices) -> R,
    {
        #[cfg(keyos)]
        unsafe {
            f(&mut *addr_of_mut!(SYSTEM_SERVICES))
        }

        #[cfg(not(keyos))]
        SYSTEM_SERVICES.with(|ss| f(&mut ss.borrow_mut()))
    }

    /// Append bytes to the panic message for the current process.
    /// If another process previously owned the buffer, ownership is transferred
    /// and the buffer is cleared.
    /// This prevents a process from blocking others from recording panic messages.
    pub fn append_panic_message(&mut self, msg_bytes: &[u8]) -> Result<(), Error> {
        use core::fmt::Write;

        let pid = current_pid();

        // If a new process is claiming the buffer, clear it and transfer ownership
        if self.panic_message_pid != Some(pid) {
            self.panic_message = BufStr::new();
            self.panic_message_pid = Some(pid);
        }

        let str = core::str::from_utf8(msg_bytes).map_err(|_| Error::InvalidString)?;
        self.panic_message.write_str(str).map_err(|_| Error::InvalidString)?;

        Ok(())
    }

    pub fn get_panic_message(&mut self) -> (Option<PID>, &[u8]) {
        (self.panic_message_pid, self.panic_message.as_slice())
    }

    /// Create a new "System Services" object based on the arguments from the
    /// kernel. These arguments decide where the memory spaces are located, as
    /// well as where the stack and program counter should initially go.
    #[cfg(keyos)]
    pub fn init_from_memory(&mut self, args: &crate::args::KernelArguments) {
        use keyos::{
            PAGE_SIZE, PID1_IRQ_HANDLER_STACK_BOTTOM, PID1_IRQ_HANDLER_STACK_PAGE_COUNT, PID1_STACK_BOTTOM,
            PID1_STACK_PAGE_COUNT,
        };
        use xous::AppId;

        const PID1: PID = PID::new(1).unwrap();

        let mut process = Process::new(
            MemoryMapping::current(),
            PID1,
            PID1,
            [0x31444950u32, 0, 0, 0].into(), // 'PID1'
        );
        process.set_thread_priority(INITIAL_TID, xous::ThreadPriority::Idle);
        process.set_thread_state(INITIAL_TID, ThreadState::Ready);
        process.set_name(b"kernel").expect("Couldn't set the PID1 name");
        process.set_syscall_permissions(u64::MAX);
        self.processes[0] = Some(process);

        let stack = unsafe {
            MemoryRange::new(
                PID1_STACK_BOTTOM - PID1_STACK_PAGE_COUNT * PAGE_SIZE,
                PID1_STACK_PAGE_COUNT * PAGE_SIZE,
            )
            .expect("stack")
        };
        let irq_stack = unsafe {
            MemoryRange::new(
                PID1_IRQ_HANDLER_STACK_BOTTOM - PID1_IRQ_HANDLER_STACK_PAGE_COUNT * PAGE_SIZE,
                PID1_IRQ_HANDLER_STACK_PAGE_COUNT * PAGE_SIZE,
            )
            .expect("irq stack")
        };
        // Set up our handle with a bogus sp and pc.  These will get updated
        // once a context switch _away_ from the kernel occurs, however we need
        // to make sure other fields such as "thread number" are all valid.
        ArchProcess::setup_process(
            crate::arch::process::ProcessSetup {
                pid: PID::new(1).unwrap(),
                entry_point: 0,
                stack,
                irq_stack,
                aslr_slide: 0,
            },
            self,
        )
        .expect("couldn't setup PID1 process");

        for arg in args.iter().filter(|arg| arg.name == u32::from_le_bytes(*b"BElf")) {
            // Restart the watchdog per process loaded.
            // This gives the entire watchdog time period for the process to load
            platform::atsama5d2::wdt::restart();

            let mut pname: [u8; MAX_PROCESS_NAME_LEN] = [0; MAX_PROCESS_NAME_LEN];

            let app_id = AppId::from([arg.data[2], arg.data[3], arg.data[4], arg.data[5]]);
            pname
                .iter_mut()
                .zip(arg.data[6..].iter().flat_map(|x| x.to_le_bytes()))
                .for_each(|(a, b)| *a = b);

            let name_len = pname.iter().position(|b| *b == 0).unwrap_or(MAX_PROCESS_NAME_LEN);
            let _name = core::str::from_utf8(&pname[..name_len]).unwrap_or("N/A");
            println!("[>] Loading `{}` size: {} k", _name, arg.data[1] / 1024);

            self.create_process(ProcessInit {
                elf: unsafe {
                    MemoryRange::new(args.base as usize + arg.data[0] as usize, arg.data[1] as usize).unwrap()
                },
                name_addr: MemoryAddress::new(&pname as *const u8 as _).unwrap(),
                app_id,
            })
            .unwrap();
        }

        for arg in args.iter() {
            if arg.name == u32::from_le_bytes(*b"PMem") {
                let pid = arg.data[0] as u8;
                for range in arg.data[1..].chunks(2) {
                    self.process_mut(PID::new(pid).unwrap())
                        .unwrap_or_else(|e| panic!("Could not find PID={pid} in arg {arg}: {e:?}"))
                        .add_memory_permission((range[0] as usize)..(range[1] as usize))
                        .unwrap_or_else(|e| panic!("Could not add memory permission to {pid}: {e:?}"));
                }
            } else if arg.name == u32::from_le_bytes(*b"PSys") {
                let pid = arg.data[0] as u8;
                let mask = (arg.data[1] as u64) | ((arg.data[2] as u64) << 32);
                self.process_mut(PID::new(pid).unwrap())
                    .unwrap_or_else(|e| panic!("Could not find PID={pid} in arg {arg}: {e:?}"))
                    .set_syscall_permissions(mask);
            }
        }
    }

    /// Add a new entry to the process table. This results in a new address space
    /// and a new PID, though the process is in the state `Setup()`.
    pub fn create_process(&mut self, init_process: ProcessInit) -> Result<ProcessStartup, Error> {
        let ppid = crate::arch::process::current_pid();
        let slot_index = self
            .processes
            .iter()
            .enumerate()
            .position(|(i, p)| p.is_none() && self.panic_message_pid != PID::new(i as u8 + 1))
            .ok_or_else(|| {
                println!("[!] No free PIDs left to allocate a new process");
                Error::KernelTableFull
            })?;
        let pid = pid_from_usize(slot_index + 1)?;
        let mut mapping = MemoryMapping::default();
        unsafe { mapping.allocate(pid)? };
        let mut process = Process::new(mapping, pid, ppid, init_process.app_id);
        #[cfg(keyos)]
        {
            let name_str = unsafe {
                core::slice::from_raw_parts(init_process.name_addr.get() as *const u8, MAX_PROCESS_NAME_LEN)
            };
            process.set_name(name_str)?;
        }
        process.set_thread_state(INITIAL_TID, ThreadState::Ready);
        self.processes[slot_index] = Some(process);
        let startup = crate::arch::process::Process::create(pid, init_process, self).unwrap();
        klog!("created new process for PID {} with PPID {}", pid, ppid);
        Ok(startup)
    }

    pub fn process_index(pid: PID) -> usize {
        // PID0 doesn't exist -- process IDs are offset by 1.
        pid.get() as usize - 1
    }

    pub fn process(&self, pid: PID) -> Result<&Process, Error> {
        self.processes.get(Self::process_index(pid)).and_then(|p| p.as_ref()).ok_or(Error::ProcessNotFound)
    }

    pub fn process_mut(&mut self, pid: PID) -> Result<&mut Process, Error> {
        self.processes
            .get_mut(Self::process_index(pid))
            .and_then(|p| p.as_mut())
            .ok_or(Error::ProcessNotFound)
    }

    pub fn free_process(&mut self, pid: PID) { self.processes[Self::process_index(pid)] = None; }

    pub fn current_process(&self) -> &Process { self.process(current_pid()).unwrap() }

    pub fn current_process_mut(&mut self) -> &mut Process { self.process_mut(current_pid()).unwrap() }

    pub fn for_all_processes(&mut self, mut f: impl FnMut(&mut Process)) {
        for process in self.processes.iter_mut().flatten() {
            f(process);
        }
    }

    pub fn retry_syscall(&mut self, tid: TID, state: ThreadState) -> Result<xous::Result, Error> {
        ArchProcess::current().retry_swi_instruction(tid)?;
        self.current_process_mut().set_thread_state(tid, state);
        Scheduler::with_mut(|s| s.activate_current(self))
    }

    /// Rewind `tid` in `pid` onto its `svc` so the parked syscall re-executes once readied.
    /// Cross-process twin of [`Self::retry_syscall`]; the caller marks the thread Ready.
    pub fn retry_syscall_pid(&mut self, pid: PID, tid: TID) -> Result<(), Error> {
        // Temporarily switch into the target process memory space to reach its thread
        // contexts, and switch back before propagating any rewind error.
        let current_pid = current_pid();
        if current_pid == pid {
            return ArchProcess::current().retry_swi_instruction(tid);
        }

        self.process(pid)?.activate();
        let result = ArchProcess::current().retry_swi_instruction(tid);

        // Return to the original memory space.
        self.process(current_pid).expect("couldn't switch back after rewinding syscall").activate();
        result
    }

    pub fn set_thread_result(&mut self, pid: PID, tid: TID, result: xous::Result) -> Result<(), Error> {
        // Temporarily switch into the target process memory space
        // in order to pass the return value.
        let current_pid = current_pid();
        if current_pid == pid {
            ArchProcess::current().set_thread_result(tid, result);
            return Ok(());
        }

        self.process(pid)?.activate();
        ArchProcess::current().set_thread_result(tid, result);

        // Return to the original memory space.
        self.process(current_pid).expect("couldn't switch back after setting context result").activate();
        Ok(())
    }

    /// Release a sender parked on a permission request that has just been invalidated,
    /// delivering `error`. The caller must pass a sender it just obtained from
    /// `tombstone_next`, which only yields still-valid requests, and a valid request always
    /// still has its sender parked (a process tombstones its own parked sends without failing
    /// them during teardown), so the sender is guaranteed to be parked on exactly this request.
    fn fail_permission_request(&mut self, sender_pid: PID, sender_tid: TID, request_id: u16, error: Error) {
        assert!(
            self.process(sender_pid).map(|p| p.thread_state(sender_tid))
                == Ok(ThreadState::RetryPermission { request_id }),
            "fail_permission_request: sender {sender_pid:?}/{sender_tid} is not parked on request {request_id}",
        );
        self.set_thread_result(sender_pid, sender_tid, xous::Result::Error(error))
            .expect("parked sender must accept its failure result");
        self.process_mut(sender_pid)
            .expect("parked sender's process must exist")
            .set_thread_state(sender_tid, ThreadState::Ready);
    }

    /// Move memory from one process to another.
    ///
    /// During this process, memory is deallocated from the first process, then
    /// we switch contexts and look for a free slot in the second process. After
    /// that, we switch back to the first process and return.
    ///
    /// If no free slot can be found, memory is re-attached to the first
    /// process.  By following this break-then-make approach, we avoid getting
    /// into a situation where memory may appear in two different processes at
    /// once.
    ///
    /// The given memory range is guaranteed to be unavailable in the src process
    /// after this function returns.
    ///
    /// # Returns
    ///
    /// Returns the virtual address of the memory region in the target process.
    ///
    /// # Errors
    ///
    /// * **ShareViolation**: Tried to mutably share a region that was already shared
    /// * **BadAddress**: The provided address was not valid
    /// * **BadAlignment**: The provided address or length was not page-aligned
    ///
    /// # Panics
    ///
    /// If the memory should have been able to go into the destination process
    /// but failed, then the system panics.
    #[cfg(keyos)]
    pub fn send_memory(
        &mut self,
        src_virt: *mut usize,
        dest_pid: PID,
        dest_virt: *mut usize,
        len: usize,
    ) -> Result<*mut usize, Error> {
        if len == 0 {
            return Err(Error::BadAddress);
        }
        if len & 0xfff != 0 {
            return Err(Error::BadAddress);
        }
        if src_virt as usize & 0xfff != 0 {
            return Err(Error::BadAddress);
        }
        if dest_virt as usize & 0xfff != 0 {
            return Err(Error::BadAddress);
        }
        if (src_virt as usize).checked_add(len).is_none_or(|end| end > keyos::USER_AREA_END) {
            return Err(Error::BadAddress);
        }
        if (dest_virt as usize).checked_add(len).is_none_or(|end| end > keyos::USER_AREA_END) {
            return Err(Error::BadAddress);
        }

        let current_pid = current_pid();

        // Iterators and `ptr.wrapping_add()` operate on `usize` types,
        // which effectively lowers the `len`.
        let usize_len = len / core::mem::size_of::<usize>();
        let usize_page = crate::mem::PAGE_SIZE / core::mem::size_of::<usize>();

        // If the dest and src PID is the same, do nothing.
        if current_pid == dest_pid {
            return Ok(src_virt);
        }

        let src_mapping = &mut self.process_mut(current_pid)?.mapping;
        // Opt out of the borrow checker, because we know these are two different mappings.
        let src_mapping = unsafe { &mut *(src_mapping as *mut _) };
        let dest_process = self.process_mut(dest_pid)?;
        let dest_virt = dest_process.find_virtual_address(dest_virt, len)?;
        let dest_mapping = &mut dest_process.mapping;
        let dest_virt = crate::mem::MemoryManager::with_mut(|mm| {
            // Move each subsequent page; move_page backs on-demand sources itself.
            for offset in (0..usize_len).step_by(usize_page) {
                assert_eq!(((src_virt.wrapping_add(offset) as usize) & 0xfff), 0);
                assert_eq!(((dest_virt.wrapping_add(offset) as usize) & 0xfff), 0);
                if let Err(e) = mm.move_page(
                    src_mapping,
                    src_virt.wrapping_add(offset),
                    dest_mapping,
                    dest_virt.wrapping_add(offset),
                ) {
                    // Move the earlier pages back so the range stays wholly in the sender.
                    for undo in (0..offset).step_by(usize_page) {
                        mm.move_page(
                            dest_mapping,
                            dest_virt.wrapping_add(undo),
                            src_mapping,
                            src_virt.wrapping_add(undo),
                        )
                        .expect("reverting a partial move must not fail");
                    }
                    return Err(e);
                }
            }
            Ok(dest_virt)
        })?;

        // The source pages have left this (the sender's) space; let its next
        // allocation reuse the freed range instead of marching past it.
        self.process_mut(current_pid)?.release_allocation_hint(src_virt as usize);

        Ok(dest_virt)
    }

    #[cfg(not(keyos))]
    pub fn send_memory(
        &mut self,
        src_virt: *mut usize,
        _dest_pid: PID,
        _dest_virt: *mut usize,
        _len: usize,
    ) -> Result<*mut usize, Error> {
        Ok(src_virt)
    }

    /// Lend memory from one process to another.
    ///
    /// During this process, memory is marked as `Shared` in the source process.
    /// If the share is Mutable, then this memory is unmapped from the source
    /// process.  If the share is immutable, then memory is marked as
    /// not-writable in the source process.
    ///
    /// If no free slot can be found, memory is re-attached to the first
    /// process.  By following this break-then-make approach, we avoid getting
    /// into a situation where memory may appear in two different processes at
    /// once.
    ///
    /// If the share is mutable and the memory is already shared, then an error
    /// is returned.
    ///
    /// # Returns
    ///
    /// Returns the virtual address of the memory region in the target process.
    ///
    /// # Errors
    ///
    /// * **ShareViolation**: Tried to mutably share a region that was already shared
    /// * **BadAddress**: The provided address was not valid
    /// * **BadAlignment**: The provided address or length was not page-aligned
    #[cfg(keyos)]
    pub fn lend_memory(
        &mut self,
        src_virt: *mut usize,
        dest_pid: PID,
        dest_virt: *mut usize,
        len: usize,
        mutable: bool,
    ) -> Result<*mut usize, Error> {
        if len == 0 {
            return Err(Error::BadAddress);
        }
        if len & 0xfff != 0 {
            return Err(Error::BadAlignment);
        }
        if src_virt as usize & 0xfff != 0 {
            return Err(Error::BadAlignment);
        }
        if dest_virt as usize & 0xfff != 0 {
            return Err(Error::BadAlignment);
        }
        // Iterators and `ptr.wrapping_add()` operate on `usize` types,
        // which effectively lowers the `len`.
        let usize_len = len / core::mem::size_of::<usize>();
        let usize_page = crate::mem::PAGE_SIZE / core::mem::size_of::<usize>();

        let current_pid = current_pid();
        // If it's within the same process, ignore the move operation and
        // just ensure the pages actually exist.
        if current_pid == dest_pid {
            MemoryManager::with_mut(|mm| {
                for offset in (0..usize_len).step_by(usize_page) {
                    assert!(((src_virt.wrapping_add(offset) as usize) & 0xfff) == 0);
                    mm.ensure_backed(src_virt.wrapping_add(offset))?;
                }
                Ok(())
            })?;
            return Ok(src_virt);
        }
        let src_mapping = &mut self.process_mut(current_pid)?.mapping;
        // Opt out of the borrow checker, because we know these are two different mappings.
        let src_mapping = unsafe { &mut *(src_mapping as *mut _) };
        let dest_process = self.process_mut(dest_pid)?;
        let dest_virt = dest_process.find_virtual_address(dest_virt, len)?;
        let dest_mapping = &mut dest_process.mapping;
        use crate::mem::MemoryManager;
        MemoryManager::with_mut(|mm| {
            // Lend each subsequent page; lend_page backs on-demand sources itself.
            for offset in (0..usize_len).step_by(usize_page) {
                assert!(((src_virt.wrapping_add(offset) as usize) & 0xfff) == 0);
                assert!(((dest_virt.wrapping_add(offset) as usize) & 0xfff) == 0);
                if let Err(e) = mm.lend_page(
                    src_mapping,
                    src_virt.wrapping_add(offset),
                    dest_mapping,
                    dest_virt.wrapping_add(offset),
                    mutable,
                ) {
                    // Unlend the earlier pages so the range stays wholly in the sender.
                    for undo in (0..offset).step_by(usize_page) {
                        mm.unlend_page(
                            dest_mapping,
                            dest_virt.wrapping_add(undo),
                            src_mapping,
                            src_virt.wrapping_add(undo),
                        )
                        .expect("reverting a partial lend must not fail");
                    }
                    return Err(e);
                }
            }
            Ok(dest_virt)
        })
    }

    #[cfg(not(keyos))]
    pub fn lend_memory(
        &mut self,
        src_virt: *mut usize,
        _dest_pid: PID,
        _dest_virt: *mut usize,
        _len: usize,
        _mutable: bool,
    ) -> Result<*mut usize, Error> {
        Ok(src_virt)
    }

    /// Return memory from one process back to another
    ///
    /// During this process, memory is unmapped from the source process.
    ///
    /// # Returns
    ///
    /// Returns the virtual address of the memory region in the target process.
    ///
    /// # Errors
    ///
    /// * **ShareViolation**: Tried to mutably share a region that was already shared
    #[cfg(keyos)]
    pub fn return_memory(
        &mut self,
        src_virt: *mut usize,
        dest_pid: PID,
        _dest_tid: TID,
        dest_virt: *mut usize,
        len: usize,
    ) -> Result<*mut usize, Error> {
        if len == 0 {
            // klog!("No len");
            return Err(Error::BadAddress);
        }
        if len & 0xfff != 0 {
            // klog!("len not aligned");
            return Err(Error::BadAddress);
        }
        if src_virt as usize & 0xfff != 0 {
            // klog!("Src virt not aligned");
            return Err(Error::BadAddress);
        }
        if dest_virt as usize & 0xfff != 0 {
            // klog!("dest virt not aligned");
            return Err(Error::BadAddress);
        }

        // Iterators and `ptr.wrapping_add()` operate on `usize` types,
        // which effectively lowers the `len`.
        let usize_len = len / core::mem::size_of::<usize>();
        let usize_page = crate::mem::PAGE_SIZE / core::mem::size_of::<usize>();

        let current_pid = current_pid();
        // If it's within the same process, ignore the operation.
        if current_pid == dest_pid {
            return Ok(src_virt);
        }
        let src_mapping = &mut self.process_mut(current_pid)?.mapping;
        // Opt out of the borrow checker, because we know these are two different mappings.
        let src_mapping = unsafe { &mut *(src_mapping as *mut _) };
        let dest_mapping = &mut self.process_mut(dest_pid)?.mapping;
        use crate::mem::MemoryManager;
        MemoryManager::with_mut(|mm| {
            let mut error = None;

            // Lend each subsequent page.
            for offset in (0..usize_len).step_by(usize_page) {
                assert!(((src_virt.wrapping_add(offset) as usize) & 0xfff) == 0);
                assert!(((dest_virt.wrapping_add(offset) as usize) & 0xfff) == 0);
                mm.unlend_page(
                    src_mapping,
                    src_virt.wrapping_add(offset),
                    dest_mapping,
                    dest_virt.wrapping_add(offset),
                )
                .unwrap_or_else(|e| {
                    error = Some(e);
                });
            }
            error.map_or_else(|| Ok(dest_virt), Err)
        })
    }

    #[cfg(not(keyos))]
    pub fn return_memory(
        &mut self,
        src_virt: *mut usize,
        _dest_pid: PID,
        _dest_tid: TID,
        _dest_virt: *mut usize,
        _len: usize,
    ) -> Result<*mut usize, Error> {
        Ok(src_virt)
    }

    /// Drop the lent (shared) reservation over `[addr, addr + len)` in `pid`'s space; see
    /// [`crate::arch::mem::MemoryMapping::clear_shared`].
    #[cfg(keyos)]
    pub fn clear_shared_range(&mut self, pid: PID, addr: MemoryAddress, len: MemorySize, mode: ClearShared) {
        let mapping = &mut self.process_mut(pid).expect("client process gone").mapping;
        for offset in (0..len.get()).step_by(crate::mem::PAGE_SIZE) {
            mapping.clear_shared((addr.get() + offset) as *mut usize, mode).unwrap();
        }
    }

    #[cfg(not(keyos))]
    pub fn clear_shared_range(
        &mut self,
        _pid: PID,
        _addr: MemoryAddress,
        _len: MemorySize,
        _mode: ClearShared,
    ) {
    }

    /// Create a new thread in the current process.  Execution begins at
    /// `entrypoint`, with the stack pointer set to `stack_pointer`.  A single
    /// argument will be passed to the new function.
    ///
    /// The return address of this thread will be `EXIT_THREAD`, which the
    /// kernel can trap on to indicate a thread exited.
    ///
    /// # Errors
    ///
    /// * **ThreadNotAvailable**: The process has used all of its context slots.
    pub fn create_thread(&mut self, parent: TID, thread_init: ThreadInit) -> Result<TID, Error> {
        let mut arch_process = ArchProcess::current();
        let tid = arch_process.find_free_thread().ok_or(Error::ThreadNotAvailable)?;

        arch_process.setup_thread(tid, thread_init)?;
        let process = self.current_process_mut();
        process.set_thread_state(tid, ThreadState::Ready);
        process.set_thread_priority(tid, process.thread_priority(parent));

        Ok(tid)
    }

    /// Destroy the given thread. Returns `true` if the PID has been updated.
    /// # Errors
    ///
    /// * **ThreadNotAvailable**: The thread does not exist in this process
    #[cfg(keyos)]
    pub fn thread_exited(&mut self, tid: TID) -> Result<xous::Result, Error> {
        self.current_process_mut().set_thread_state(tid, ThreadState::Free);

        if tid != crate::process::IRQ_TID {
            let mut arch_process = ArchProcess::current();

            let (return_value, stack) = arch_process.destroy_thread(tid).unwrap_or_default();
            if let Some(stack) = stack {
                crate::mem::MemoryManager::with_mut(|mm| mm.unmap_range(stack.as_ptr(), stack.len())).ok();
            }

            for waiting_tid in 0..MAX_THREAD_COUNT {
                if (self.current_process().thread_state(waiting_tid) == ThreadState::WaitJoin { tid }) {
                    self.current_process_mut().set_thread_state(waiting_tid, ThreadState::Ready);
                    self.set_thread_result(current_pid(), waiting_tid, xous::Result::Scalar1(return_value))?;
                }
            }
        }
        Scheduler::with_mut(|s| s.activate_current(self))
    }

    /// Park this thread if the target thread is currently running. Otherwise,
    /// return the value of the given thread.
    pub fn join_thread(&mut self, tid: TID, join_tid: TID) -> Result<xous::Result, Error> {
        let process = self.current_process_mut();

        // We cannot wait on ourselves.
        if tid == join_tid {
            return Err(Error::ThreadNotAvailable);
        }

        // join_tid is caller-supplied and indexes a fixed-size thread array.
        if join_tid >= MAX_THREAD_COUNT {
            return Err(Error::ThreadNotAvailable);
        }

        if process.thread_state(join_tid) != ThreadState::Free {
            process.set_thread_state(tid, ThreadState::WaitJoin { tid: join_tid });
            Scheduler::with_mut(|s| s.activate_current(self))
        } else {
            // The thread does not exist -- continue execution
            // Err(xous::Error::ThreadNotAvailable)
            Ok(xous::Result::Scalar1(0))
        }
    }

    pub fn wake_threads_with_state(&mut self, state: ThreadState, n: usize) {
        self.for_all_processes(|p| p.wake_threads_with_state(state, n));
    }

    /// Allocate a new server ID for this process and return the address. If the
    /// server table is full, or if there is not enough memory to map the server queue,
    /// return an error.
    ///
    /// # Errors
    ///
    /// * **OutOfMemory**: A new page could not be assigned to store the server queue.
    /// * **KernelTableFull**: The server table was full and a free slot could not be found.
    pub fn create_server_with_address(
        &mut self,
        sid: SID,
        initial_permissions: core::ops::Range<xous::MessageId>,
    ) -> Result<SID, Error> {
        let pid = current_pid();
        for entry in self.servers.iter_mut() {
            if entry.is_none() {
                #[cfg(keyos)]
                // Allocate a single page for the server queue
                let backing = crate::mem::MemoryManager::with_mut(|mm| {
                    mm.map_range(
                        0,
                        core::ptr::null_mut(),
                        crate::mem::PAGE_SIZE,
                        xous::MemoryFlags::W | xous::MemoryFlags::POPULATE,
                        false,
                    )
                })?;

                #[cfg(not(keyos))]
                let backing = unsafe { MemoryRange::new(4096, 4096).unwrap() };

                Server::init(entry, pid, sid, backing, initial_permissions).unwrap();

                self.wake_threads_with_state(
                    ThreadState::RetryConnect { sid_hash: sid.quick_hash() },
                    usize::MAX,
                );
                return Ok(sid);
            }
        }
        Err(Error::KernelTableFull)
    }

    /// Generate a random server ID and return it to the caller. Doesn't create
    /// any processes.
    pub fn create_server_id(&mut self) -> Result<SID, Error> {
        let sid = SID::from_u32(
            platform::rand::get_u32(),
            platform::rand::get_u32(),
            platform::rand::get_u32(),
            platform::rand::get_u32(),
        );
        Ok(sid)
    }

    /// Destroy the provided server ID and disconnect any processes that are
    /// connected.
    pub fn destroy_server(&mut self, pid: PID, sid: SID) -> Result<(), Error> {
        let sidx = self.sidx_from_sid(sid, pid).ok_or(Error::ServerNotFound)?;
        self.destroy_sidx(sidx);
        Ok(())
    }

    /// Connect the specified PID to the specified server
    pub fn connect_to_server(&mut self, pid: PID, sid: SID) -> Result<CID, Error> {
        let sidx = self
            .servers
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.sid == sid))
            .ok_or(Error::ServerNotFound)?;
        let permissions = self.server_from_sidx(sidx).unwrap().default_permissions.clone();
        let (cid, new_connection) = self.process_mut(pid)?.add_connection(sidx, permissions)?;
        if new_connection {
            self.send_server_event(sidx, ServerEvent::NewConnection, Default::default())?;
        }
        Ok(cid)
    }

    /// Invalidate the provided connection ID.
    pub fn disconnect_from_server(&mut self, cid: CID) -> Result<(), Error> {
        // Check to see if we've already connected to this server.
        // While doing this, find a free slot in case we haven't
        // yet connected.
        let connection_slot = self.current_process_mut().connection_mut(cid)?;
        match connection_slot {
            ConnectionSlot::Free => return Err(Error::ServerNotFound),
            ConnectionSlot::Tombstone { refcount } | ConnectionSlot::Connected { refcount, .. }
                if *refcount > 1 =>
            {
                *refcount -= 1
            }
            ConnectionSlot::Tombstone { .. } => {
                *connection_slot = ConnectionSlot::Free;
                klog!("Removing server from connection map");
            }
            ConnectionSlot::Connected { sidx, .. } => {
                let sidx = *sidx as usize;
                *connection_slot = ConnectionSlot::Free;
                // A permission request parked on this slot must never be granted to whatever
                // the slot is reconnected to; release each waiting sender now with an error
                // rather than leave it blocked on a connection that is gone.
                let disconnecting_pid = current_pid();
                while let Some((s_pid, s_tid, id)) = self
                    .permission_requests
                    .tombstone_next(|entry| entry.sender_pid == disconnecting_pid && entry.cid == cid)
                {
                    self.fail_permission_request(s_pid, s_tid, id, Error::ServerNotFound);
                }
                self.send_server_event(sidx, ServerEvent::Disconnected, Default::default())?;
                klog!("Removing server from connection map");
            }
        };
        Ok(())
    }

    /// Retrieve the server ID index from the specified SID and PID
    pub fn sidx_from_sid(&self, sid: SID, pid: PID) -> Option<usize> {
        self.servers.iter().position(|s| s.as_ref().is_some_and(|s| s.sid == sid && s.pid == pid))
    }

    /// Return a server based on the connection id and the current process
    pub fn server_from_sidx(&self, sidx: usize) -> Option<&Server> {
        if sidx >= self.servers.len() {
            None
        } else {
            self.servers[sidx].as_ref()
        }
    }

    /// Return a server based on the connection id and the current process
    pub fn server_from_sidx_mut(&mut self, sidx: usize) -> Option<&mut Server> {
        if sidx >= self.servers.len() {
            None
        } else {
            self.servers[sidx].as_mut()
        }
    }

    /// Switch to the server's memory space and add the message to its server
    /// queue
    pub fn queue_server_message(
        &mut self,
        sidx: usize,
        pid: PID,
        thread: TID,
        message: Message,
        original_address: Option<MemoryAddress>,
    ) -> Result<usize, Error> {
        let current_pid = current_pid();
        let result = {
            let server_pid = self.server_from_sidx(sidx).ok_or(Error::ServerNotFound)?.pid;
            {
                let server_process = self.process(server_pid)?;
                server_process.mapping.activate();
            }
            let server = self.server_from_sidx_mut(sidx).expect("couldn't re-discover server index");
            server.queue_message(pid, thread, message, original_address)
        };
        let current_process = self.process(current_pid).expect("couldn't restore previous process");
        current_process.mapping.activate();
        result
    }

    /// Switch to the server's address space and add a "remember this address"
    /// entry to its server queue, then switch back to the original address space.
    pub fn remember_server_message(
        &mut self,
        sidx: usize,
        current_pid: PID,
        current_thread: TID,
        message: &Message,
        client_address: Option<MemoryAddress>,
    ) -> Result<usize, Error> {
        let server_pid = self.server_from_sidx(sidx).ok_or(Error::ServerNotFound)?.pid;
        self.process(server_pid)?.mapping.activate();
        let server = self.server_from_sidx_mut(sidx).expect("couldn't re-discover server index");
        let result = server.queue_response(current_pid, current_thread, message, client_address);
        self.process(current_pid).expect("couldn't find old process").mapping.activate();
        result
    }

    /// Terminate the given process. Returns the process' parent PID.
    pub fn terminate_current_process(&mut self, ret: u32) -> Result<xous::Result, Error> {
        let pid = current_pid();

        // Notify the parent process that this process is terminating
        // Crash the OS if the terminated process was a system process
        if ret != 0 && self.current_process().ppid.map(|p| p.get() == 1).unwrap_or(false) {
            #[cfg(keyos)]
            {
                crate::arch::backtrace::capture_current_process_backtrace();
                #[cfg(not(feature = "production"))]
                crate::debug::serial::with_output(|stream| self.print_current_process(stream).unwrap());
                let process_name = self.current_process().name().unwrap_or("N/A");
                panic!("System process PID={} (`{}`) terminated with code {}", pid, process_name, ret);
            }

            #[cfg(not(keyos))]
            {
                let (panic_pid, panic_message) = self.get_panic_message();
                if panic_pid == Some(pid) {
                    panic!(
                        "System process PID={} terminated with code {}\n{}",
                        pid,
                        ret,
                        str::from_utf8(panic_message).unwrap_or("non-utf8 panic")
                    );
                } else {
                    panic!("System process PID={} terminated with code {}\n= <NO PANIC> =", pid, ret);
                }
            }
        }

        for sidx in 0..self.servers.len() {
            let Some(server_pid) = self.servers[sidx].as_ref().map(|s| s.pid) else { continue };
            if server_pid == pid {
                // This is our server, just destroy it.
                self.destroy_sidx(sidx);
            } else {
                self.process(server_pid).unwrap().activate();
                // Look through this server's memory space to determine if this process
                // is mentioned there as having some memory lent out.
                self.servers[sidx].as_mut().unwrap().discard_messages_for_pid(pid);
                self.process(pid)?.activate();
            }
        }

        // destroy_sidx above already released (via fail_permission_request) every sender
        // parked on a server this process hosted. Now tombstone this process's own parked
        // sends so a recycled pid can't match an old request; no wake, since its threads are
        // being torn down anyway.
        while self.permission_requests.tombstone_next(|entry| entry.sender_pid == pid).is_some() {}

        if let Some(ppid) = self.current_process().ppid {
            self.send_system_event(ppid, SystemEvent::ChildTerminated, [ret as _, 0, 0, 0]).ok();
        }
        for sidx in self.current_process().connected_sidxes() {
            self.send_server_event(sidx, ServerEvent::Disconnected, Default::default()).ok();
        }

        #[cfg(keyos)]
        if ret != 0 {
            crate::arch::backtrace::capture_current_process_backtrace();
            #[cfg(not(feature = "production"))]
            crate::debug::serial::with_output(|stream| self.print_current_process(stream).unwrap());
        }

        // Stop any in-flight DMA page-zeroing owned by this process before terminate() reclaims its pages,
        // or the DMA would keep writing zeros into memory handed to another process. The owning thread is in
        // this process and gets freed below; only the waiters in other processes need waking.
        #[cfg(keyos)]
        crate::mem::MemoryManager::with_mut(|mm| {
            if mm.map_zero_ongoing() == Some(pid) {
                mm.cancel_map_zero();
                self.wake_threads_with_state(ThreadState::RetryMapZero, usize::MAX);
            }
        });

        self.process_mut(pid)?.terminate(ret)?;
        self.free_process(pid);

        // Reparent all children to PID1
        self.for_all_processes(|p| {
            if p.ppid == Some(pid) {
                p.ppid = None
            }
        });
        // In case the process terminated itself
        Scheduler::with_mut(|s| s.activate_current(self))
    }

    fn destroy_sidx(&mut self, sidx: usize) {
        // A permission request parked on this server can never be granted once the slot is
        // gone or recycled, even if the sender later reconnects the same cid elsewhere.
        // Release each waiting sender now, before the server is torn down below.
        while let Some((s_pid, s_tid, id)) =
            self.permission_requests.tombstone_next(|entry| entry.sidx == sidx)
        {
            self.fail_permission_request(s_pid, s_tid, id, Error::ServerNotFound);
        }

        // Return and dequeue any remaining messages
        self.servers[sidx].take().unwrap().destroy(self);

        // Tombstone connections, so send_message throws an error when trying to use this CID, and tell the
        // processes the server no longer exists.
        for pidx in 0..self.processes.len() {
            // Manual indexing because send_event borrow-checks the whole object as mut
            let Some(process) = self.processes[pidx].as_mut() else { continue };
            if let Some(cid) = process.tombstone_connection_by_sidx(sidx) {
                let pid = process.pid;
                self.send_system_event(pid, SystemEvent::Disconnected, [cid as usize, 0, 0, 0]).ok();
            }
        }
    }

    fn send_server_event(&mut self, sidx: usize, event: ServerEvent, args: [usize; 4]) -> Result<(), Error> {
        if let Some(server) = self.server_from_sidx_mut(sidx) {
            if let Some(id) = server.get_event_handler(event) {
                let msg = Message::new_scalar(id, args[0], args[1], args[2], args[3]);
                crate::syscall::send_message_inner(self, 0, sidx, msg)?;
            }
        }
        Ok(())
    }

    /// Deliver `event` to `dst_pid` if it registered a handler for it. Returns `true` when a
    /// handler received the message, `false` when the process has no handler for this event.
    pub fn send_system_event(
        &mut self,
        dst_pid: PID,
        event: SystemEvent,
        args: [usize; 4],
    ) -> Result<bool, Error> {
        if let Some((sid, id)) = self.process(dst_pid)?.get_event_handler(event) {
            if let Some(sidx) = self.sidx_from_sid(sid, dst_pid) {
                let msg = Message::new_scalar(id, args[0], args[1], args[2], args[3]);
                crate::syscall::send_message_inner(self, 0, sidx, msg)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Deliver `event` to every process subscribed to it, returning how many actually received
    /// it. A return of `0` means no handler is registered anywhere, which lets callers avoid
    /// waiting on a response that can't come.
    pub fn broadcast_event(&mut self, event: SystemEvent, args: [usize; 4]) -> Result<usize, Error> {
        let mut delivered = 0;
        for pid in 1..=MAX_PROCESS_COUNT as u8 {
            let pid = PID::new(pid).unwrap();
            if self.process(pid).is_ok() && self.send_system_event(pid, event, args)? {
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    /// Terminates the process with the given PID and return code.
    pub fn terminate_process(&mut self, caller_tid: TID, pid: PID, ret: u32) -> Result<xous::Result, Error> {
        let caller_pid = current_pid();
        klog!("Terminating process with PID {pid} from PID {caller_pid}");
        // Disallow termination of processes spawned by the kernel
        if self.process(pid)?.ppid.map(|pid| pid.get() == 1).unwrap_or(false) {
            println!("[!] PID {caller_pid} attempted to terminate a system process with PID {pid}");
            return Err(Error::AccessDenied);
        }

        self.set_thread_result(caller_pid, caller_tid, xous::Result::Ok)?;
        self.process_mut(pid).unwrap().activate();
        self.terminate_current_process(ret)
    }

    /// Calls the provided function with the current inner process state.
    pub fn shutdown(&mut self) -> Result<(), Error> {
        #[cfg(keyos)]
        platform::atsama5d2::shutdown();

        // Destroy all processes. This will cause them to immediately terminate.
        #[cfg(not(keyos))]
        {
            for process in &mut self.processes {
                if let Some(process) = process {
                    process.activate();
                    process.terminate(0).unwrap();
                }
            }

            Ok(())
        }
    }

    #[cfg(all(keyos, any(not(feature = "production"), feature = "log-serial")))]
    pub fn print_current_process(&self, mut output: impl core::fmt::Write) -> Result<(), Error> {
        let process = self.current_process();
        writeln!(output, "{:x?} [{}]", process, process.name().unwrap_or("")).ok();
        crate::arch::process::Process::with_current(|arch_process| {
            for tid in 0..MAX_THREAD_COUNT {
                let thread = process.thread_state(tid);
                if thread == ThreadState::Free {
                    continue;
                }
                write!(output, "Thread {tid} (priority={:?}): ", process.thread_priority(tid)).ok();
                if tid == arch_process.current_tid() {
                    write!(output, "[Last active] ").ok();
                }
                match thread {
                    ThreadState::Free => unreachable!(),
                    ThreadState::Ready => writeln!(output,).ok(),
                    ThreadState::WaitJoin { tid: _tid } => writeln!(output, "WaitingJoin({_tid})").ok(),
                    ThreadState::RetryConnect { sid_hash: _sid_hash } => {
                        writeln!(output, "RetryConnect({_sid_hash:08x})").ok()
                    }
                    ThreadState::RetryQueueFull { sidx } => {
                        if let Some(_server) = self.server_from_sidx(sidx) {
                            writeln!(output, "RetryQueueFull({:08x?}, pid={})", _server.sid, _server.pid).ok()
                        } else {
                            writeln!(output, "RetryQueueFull(NONEXISTENT)").ok()
                        }
                    }
                    ThreadState::RetryPermission { request_id } => {
                        writeln!(output, "RetryPermission(request_id={request_id})").ok()
                    }
                    ThreadState::WaitBlocking { sidx } => {
                        if let Some(_server) = self.server_from_sidx(sidx) {
                            writeln!(output, "WaitBlocking({:08x?}, pid={})", _server.sid, _server.pid).ok()
                        } else {
                            writeln!(output, "WaitBlocking(NONEXISTENT)").ok()
                        }
                    }
                    ThreadState::WaitReceive { sidx } => {
                        if let Some(_server) = self.server_from_sidx(sidx) {
                            writeln!(output, "WaitRecv({:08x?}, pid={})", _server.sid, _server.pid).ok()
                        } else {
                            writeln!(output, "WaitRecv(NONEXISTENT)").ok()
                        }
                    }
                    ThreadState::WaitFutex { addr: _addr } => writeln!(output, "WaitFutex({_addr:08x})").ok(),
                    ThreadState::WaitMapZero => writeln!(output, "WaitMapZero").ok(),
                    ThreadState::RetryMapZero => writeln!(output, "RetryMapZero").ok(),
                };
                writeln!(output, "\tASLR:  {:08x}", process.aslr_slide).ok();
                write!(output, "{:?}", arch_process.thread(tid)).ok();
            }
        });
        writeln!(output,).ok();
        Ok(())
    }

    pub fn pid_from_app_id(&self, app_id: AppId) -> Option<PID> {
        for process in self.processes.iter().flatten() {
            if process.app_id() == app_id {
                return Some(process.pid);
            }
        }
        None
    }
}
