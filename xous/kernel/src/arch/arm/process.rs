// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

use core::mem;
use core::sync::atomic::AtomicU8;

use keyos::{
    BOOT_SPLASH_FB, BOOT_SPLASH_PAGES, PAGE_SIZE, RAW_ELF_TEMPORARY_ADDRESS, STACK_PAGE_COUNT,
    THREAD_CONTEXT_AREA, USER_IRQ_STACK_BOTTOM, USER_IRQ_STACK_PAGE_COUNT, USER_STACK_BOTTOM,
};
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use xous::MemoryFlags;
use xous::{MemoryRange, ProcessInit, ProcessStartup, ThreadInit, PID, TID};

use crate::arch::arm::elf;
use crate::mem::MemoryManager;
use crate::platform::atsama5d2::cache::{clean_cache_l1, invalidate_instruction_cache};
use crate::process::{INITIAL_TID, IRQ_TID};

static mut PROCESS: *mut ProcessImpl = THREAD_CONTEXT_AREA as *mut ProcessImpl;
pub const MAX_THREAD_COUNT: TID = 7;
pub const MAX_PROCESS_COUNT: usize = 64;

/// This is the address a thread will return to when it exits.
pub const EXIT_THREAD: usize = 0xff80_6000;

pub const PROCESSOR_MODE_MASK: usize = 0x1f;

const GUI_SERVER_APP_ID: xous::AppId = xous::AppId([
    0x67, 0x75, 0x69, 0x2d, 0x73, 0x65, 0x72, 0x76, 0x65, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
]);

pub struct ProcessSetup {
    pub pid: PID,
    pub entry_point: usize,
    pub stack: MemoryRange,
    pub irq_stack: MemoryRange,
    pub aslr_slide: usize,
}

// ProcessImpl occupies a multiple of pages mapped to virtual address `0xff80_4000` (THREAD_CONTEXT_AREA).
// Each thread is 256 bytes (64 4-byte registers). The first "thread" does not exist,
// and instead is any bookkeeping information related to the process.
#[derive(Debug, Clone)]
#[repr(C)]
struct ProcessImpl {
    /// Used by the interrupt handler to calculate offsets
    scratch: usize,

    /// The currently-active thread for this process. This must
    /// be the 2nd item, because the ISR directly accesses this value.
    hardware_thread: usize,

    /// The last thread ID that was allocated
    last_tid_allocated: u8,

    /// Pad everything to size_of::<Thread>() bytes, so the Thread slice starts at
    /// an offset of 1 x Thread.
    _padding: [u32; 125],

    /// This enables the kernel to keep track of threads in the
    /// target process, and know which threads are ready to
    /// receive messages.
    threads: [Thread; MAX_THREAD_COUNT],
}

// A compile-time check that the process structure doesn't overflow
const _: () = {
    if mem::size_of::<ProcessImpl>() != PAGE_SIZE {
        const _SIZE: [u8; mem::size_of::<ProcessImpl>()] = [0; PAGE_SIZE]; // This will produce a compile error with the actual size
        panic!("Incorrect size of ProcessImpl structure. Ensure correct padding");
    }
};

static CURRENT_PROCESS: AtomicU8 = AtomicU8::new(1);

pub fn set_current_pid(pid: PID) { CURRENT_PROCESS.store(pid.get(), core::sync::atomic::Ordering::SeqCst); }

pub fn current_pid() -> PID {
    PID::try_from(CURRENT_PROCESS.load(core::sync::atomic::Ordering::SeqCst)).unwrap()
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Process {
    pid: PID,
}

impl Process {
    pub fn current() -> Process {
        // TODO: find a place where to call `set_hardware_pid()` for this to not panic
        //let hardware_pid = unsafe { get_hardware_pid() & 0xff }; // Discards the process ID field of
        // CONTEXTIDR assert_eq!((pid.get() as usize), hardware_pid,
        //           "Hardware current PID doesn't match the software. hw = {} vs sw = {}", pid,
        // hardware_pid);
        Process { pid: current_pid() }
    }

    /// Calls the provided function with the current inner process state.
    #[allow(dead_code)]
    pub fn with_current<F, R>(f: F) -> R
    where
        F: FnOnce(&Process) -> R,
    {
        let process = Self::current();
        f(&process)
    }

    /// Calls the provided function with the current inner process state.
    pub fn with_current_mut<F, R>(f: F) -> R
    where
        F: FnOnce(&mut Process) -> R,
    {
        let mut process = Self::current();
        f(&mut process)
    }

    pub fn current_thread_mut(&mut self) -> &mut Thread {
        let process = unsafe { &mut *PROCESS };
        assert!(process.hardware_thread != 0, "thread number was 0");
        &mut process.threads[process.hardware_thread - 1]
    }

    pub fn current_thread(&self) -> &Thread {
        let process = unsafe { &mut *PROCESS };
        assert!(process.hardware_thread != 0, "thread number was 0");
        &mut process.threads[process.hardware_thread - 1]
    }

    pub fn current_tid(&self) -> TID {
        let process = unsafe { &*PROCESS };
        process.hardware_thread - 1
    }

    /// Set the current thread number.
    pub fn set_tid(&mut self, thread: TID) {
        let process = unsafe { &mut *PROCESS };
        klog!("Switching to thread {}", thread);
        assert!(thread <= process.threads.len(), "attempt to switch to an invalid thread {}", thread);
        process.hardware_thread = thread + 1;
    }

    pub fn thread_mut(&mut self, thread: TID) -> &mut Thread {
        let process = unsafe { &mut *PROCESS };
        assert!(thread <= process.threads.len(), "attempt to retrieve an invalid thread {}", thread);
        &mut process.threads[thread]
    }

    #[cfg(any(not(feature = "production"), feature = "log-serial"))]
    pub fn thread(&self, thread: TID) -> &Thread {
        let process = unsafe { &mut *PROCESS };
        assert!(thread <= process.threads.len(), "attempt to retrieve an invalid thread {}", thread);
        &process.threads[thread]
    }

    pub fn find_free_thread(&self) -> Option<TID> {
        let process = unsafe { &mut *PROCESS };
        let start_tid = process.last_tid_allocated as usize;
        let a = &process.threads[start_tid..process.threads.len()];
        let b = &process.threads[0..start_tid];
        for (index, thread) in a.iter().chain(b.iter()).enumerate() {
            let mut tid = index + start_tid;
            if tid >= process.threads.len() {
                tid -= process.threads.len()
            }

            if tid != IRQ_TID && thread.pc == 0 {
                process.last_tid_allocated = tid as _;
                return Some(tid as TID);
            }
        }
        None
    }

    pub fn set_thread_result(&mut self, thread_nr: TID, result: xous::Result) {
        klog!("Setting TID={thread_nr} result: {result:?}");
        self.thread_mut(thread_nr).set_args(result.to_args());
    }

    pub fn retry_swi_instruction(&mut self, tid: TID) -> Result<(), xous::Error> {
        let process = unsafe { &mut *PROCESS };
        let thread = &mut process.threads[tid];
        thread.pc = thread.previous_instr_pc();
        Ok(())
    }

    /// Initialize this process thread with the given entrypoint and stack
    /// addresses.
    pub fn setup_process(
        setup: ProcessSetup,
        services: &mut crate::SystemServices,
    ) -> Result<(), xous::Error> {
        let process = unsafe { &mut *PROCESS };
        if setup.pid.get() > 1 {
            assert_eq!(setup.pid, crate::arch::current_hw_pid(), "hardware pid does not match setup pid");
        }

        klog!(
            "initializing PID {} with entrypoint {:08x}, stack @ {:08x?}",
            setup.pid,
            setup.entry_point,
            setup.stack
        );
        let size = mem::size_of::<ProcessImpl>();
        assert_eq!(
            size,
            PAGE_SIZE,
            "Process size is {}, not PAGE_SIZE ({}) (Thread size: {}, array: {})",
            mem::size_of::<ProcessImpl>(),
            PAGE_SIZE,
            mem::size_of::<Thread>(),
            mem::size_of::<[Thread; MAX_THREAD_COUNT + 1]>(),
        );

        // By convention, thread 0 is the trap thread. Therefore, thread 1 is
        // the first default thread. There is an offset of 1 due to how the
        // interrupt handler functions.
        process.hardware_thread = INITIAL_TID + 1;

        // Reset the thread state, since it's possibly uninitialized memory
        for thread in process.threads.iter_mut() {
            *thread = Default::default();
        }

        let processor_mode = if setup.pid.get() == 1 { ProcessorMode::System } else { ProcessorMode::User };
        let initial_thread = &mut process.threads[INITIAL_TID];
        initial_thread.init_process_params();
        initial_thread.set_processor_mode(processor_mode);
        if setup.pid.get() == 1 {
            initial_thread.disable_interrupts();
        }
        initial_thread.set_pc(setup.entry_point);
        initial_thread.lr = EXIT_THREAD;
        initial_thread.stack = Some(setup.stack);
        initial_thread.sp = setup.stack.as_ptr() as usize + setup.stack.len();

        let irq_thread = &mut process.threads[IRQ_TID];
        irq_thread.set_processor_mode(processor_mode);
        irq_thread.disable_interrupts();
        irq_thread.stack = Some(setup.irq_stack);
        irq_thread.sp = setup.irq_stack.as_ptr() as usize + setup.irq_stack.len();

        // Store ASLR slide for backtraces (only used after a crash)
        services.process_mut(setup.pid).unwrap().aslr_slide = setup.aslr_slide;

        #[cfg(feature = "trace-systemview")]
        {
            if setup.pid.get() != 1 {
                let name = services.process(setup.pid).expect("process").name().expect("process name");
                systemview_keyos::SystemView::thread_send_info(setup.pid, INITIAL_TID, name, setup.stack);
            }
        }
        Ok(())
    }

    pub fn setup_thread(&mut self, new_tid: TID, setup: ThreadInit) -> Result<(), xous::Error> {
        assert_ne!(self.pid.get(), 1, "PID1 should not spawn threads after init");

        let thread = self.thread_mut(new_tid);
        thread.set_processor_mode(ProcessorMode::User);
        thread.set_pc(setup.call);
        thread.lr = EXIT_THREAD;
        thread.stack = Some(setup.stack);
        thread.sp = setup.stack.as_ptr() as usize + setup.stack.len();
        thread.r0 = setup.arg1;
        thread.r1 = setup.arg2;
        thread.r2 = setup.arg3;
        thread.r3 = setup.arg4;

        #[cfg(feature = "trace-systemview")]
        {
            const MAX_THREAD_NAME_LEN: usize = xous::arch::MAX_PROCESS_NAME_LEN + 16;
            use core::fmt::Write;

            use crate::debug::BufStr;
            crate::SystemServices::with(|ss| {
                let name = ss.process(self.pid).expect("process").name().expect("process name");
                let mut process_with_thread = BufStr::<[u8; MAX_THREAD_NAME_LEN]>::new();
                write!(process_with_thread, "{} (thread {})", name, new_tid).ok();
                let process_with_thread_str =
                    core::str::from_utf8(process_with_thread.as_slice()).unwrap_or(name);
                systemview_keyos::SystemView::thread_send_info(
                    self.pid,
                    new_tid,
                    process_with_thread_str,
                    setup.stack,
                );
            });
        }

        Ok(())
    }

    pub fn run_irq_handler(&mut self, pc: usize, irq_no: usize, arg: usize) {
        let thread = self.thread_mut(IRQ_TID);
        let stack = thread.stack.unwrap();
        thread.set_pc(pc);
        thread.sp = stack.as_ptr() as usize + stack.len();
        thread.lr = EXIT_THREAD;
        thread.r0 = irq_no;
        thread.r1 = arg;

        self.set_tid(IRQ_TID);
    }

    /// Destroy a given thread and return its return value.
    ///
    /// # Returns
    ///     - The return value of the function
    ///     - The stack area
    ///
    /// # Errors
    ///     xous::ThreadNotAvailable - the thread did not exist
    pub fn destroy_thread(&mut self, tid: TID) -> Result<(usize, Option<MemoryRange>), xous::Error> {
        let thread = self.thread_mut(tid);
        let stack = thread.stack;

        // Ensure this thread is valid
        if thread.sp == 0 || tid == IRQ_TID {
            return Err(xous::Error::ThreadNotAvailable);
        }
        let return_value = thread.r0;
        thread.clean();

        Ok((return_value, stack))
    }

    /// Create a brand-new process. The memory space must already be set up.
    pub fn create(
        pid: PID,
        init_data: ProcessInit,
        services: &mut crate::SystemServices,
    ) -> Result<ProcessStartup, xous::Error> {
        services.send_memory(
            init_data.elf.as_ptr() as *mut usize,
            pid,
            RAW_ELF_TEMPORARY_ADDRESS as *mut usize,
            init_data.elf.len(),
        )?;

        let current_pid = current_pid();
        services.process(pid)?.activate();

        let stack = unsafe {
            MemoryRange::new(USER_STACK_BOTTOM - STACK_PAGE_COUNT * PAGE_SIZE, STACK_PAGE_COUNT * PAGE_SIZE)
                .expect("stack")
        };
        let irq_stack = unsafe {
            MemoryRange::new(
                USER_IRQ_STACK_BOTTOM - USER_IRQ_STACK_PAGE_COUNT * PAGE_SIZE,
                USER_IRQ_STACK_PAGE_COUNT * PAGE_SIZE,
            )
            .expect("irq stack")
        };
        MemoryManager::with_mut(|mm| {
            mm.map_range(
                0,
                THREAD_CONTEXT_AREA as _,
                PAGE_SIZE,
                MemoryFlags::W | MemoryFlags::POPULATE,
                false,
            )?;
            mm.map_range(0, stack.as_mut_ptr() as _, stack.len(), MemoryFlags::W, true)?;
            mm.map_range(0, irq_stack.as_mut_ptr() as _, irq_stack.len(), MemoryFlags::W, true)
        })?;
        let elf::ElfLoadResult { entry_point, aslr_slide } =
            elf::load_elf(RAW_ELF_TEMPORARY_ADDRESS, init_data.elf.len())?;
        // We have a Virtually Indexed, Physically Tagged cache, which only needs to be flushed whenever we
        // write the actual RAM pages with data. We just loaded executable pages above, so let's do that.
        clean_cache_l1();
        invalidate_instruction_cache();

        Self::setup_process(ProcessSetup { pid, entry_point, stack, irq_stack, aslr_slide }, services)
            .unwrap();

        // XXX: Map the boot splash screen to the gui-server so it can free it when no longer needed.
        //      This is not exactly pretty, but there aren't many other ways to make sure the FB doesn't
        //      get clobbered.
        if init_data.app_id == GUI_SERVER_APP_ID {
            let src_mapping = &mut services.process_mut(PID::new(1).unwrap())?.mapping;
            // Opt out of the borrow checker, because we know these are two different mappings.
            let src_mapping = unsafe { &mut *(src_mapping as *mut _) };
            let dest_mapping = &mut services.process_mut(pid)?.mapping;
            MemoryManager::with_mut(|mm| {
                for page in 0..BOOT_SPLASH_PAGES {
                    let addr = (BOOT_SPLASH_FB + page * PAGE_SIZE) as _;
                    mm.move_page(src_mapping, addr, dest_mapping, addr)?
                }
                Ok(())
            })?
        }

        services.process(current_pid)?.activate();

        Ok(ProcessStartup::new(pid))
    }

    pub fn destroy(_pid: PID) -> Result<(), xous::Error> { Ok(()) }
}

/// Implementation behind [`crash_current_process!`]; callers should use the macro.
pub(crate) fn crash_current_process_fmt(args: core::fmt::Arguments<'_>) {
    use core::fmt::Write;

    let mut msg = crate::debug::BufStr::<[u8; 256]>::new();
    let _ = msg.write_fmt(args);
    println!("{}", msg);
    crate::SystemServices::with_mut(|ss| ss.append_panic_message(msg.as_slice()).ok());

    crate::SystemServices::with_mut(|ss| {
        ss.terminate_current_process(255).expect("couldn't terminate the process");
    });

    // Resume the next scheduled process
    crate::services::ArchProcess::with_current_mut(|process| {
        crate::arch::irq::resume(process.current_thread_mut())
    })
}

/// Logs a formatted message to serial and to the process's panic-message buffer
/// (so it surfaces alongside a userspace `panic!()` in the log service), then
/// terminates the current process. `terminate_current_process` captures the
/// backtrace on the way out.
macro_rules! crash_current_process {
    ($($arg:tt)*) => {
        $crate::arch::process::crash_current_process_fmt(::core::format_args!($($arg)*))
    };
}
pub(crate) use crash_current_process;

/// Everything required to keep track of a single thread of execution.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Thread {
    pub r0: usize,  // 0
    pub r1: usize,  // 1
    pub r2: usize,  // 2
    pub r3: usize,  // 3
    pub r4: usize,  // 4
    pub r5: usize,  // 5
    pub r6: usize,  // 6
    pub r7: usize,  // 7
    pub r8: usize,  // 8
    pub r9: usize,  // 9
    pub r10: usize, // 10
    pub fp: usize,  // 11
    pub ip: usize,  // 12
    pub sp: usize,  // 13
    pub lr: usize,  // 14
    pub pc: usize,  // 15
    pub psr: usize, // 16

    /// A hardware "thread pointer" for TLS (see ARM ARM B3.12.46)
    pub tp: usize, // 17

    pub fpscr: usize,         // 18
    pub s0_s31: [usize; 32],  // 19
    pub s32_s63: [usize; 32], // 51

    /// A virtual memory range where the allocated stack is
    pub stack: Option<MemoryRange>,

    // Pad to 512 bytes in size
    _padding: [usize; 43],
}

// A compile-time check that the thread structure doesn't overflow
const _: () = {
    if mem::size_of::<Thread>() != 512 {
        const _SIZE: [usize; 512] = [0; mem::size_of::<Thread>()]; // This will produce a helpful compile error with the actual size
        panic!("Incorrect size of Thread structure. Ensure correct padding");
    }
};

impl Thread {
    /// Zeroes thread's context data.
    fn clean(&mut self) { *self = Default::default(); }

    pub fn is_in_thumb_mode(&self) -> bool { self.psr & (1 << 5) != 0 }

    /// PC of the instruction preceding the saved return address. To be used for
    /// the SVC and Undefined exceptions only.
    pub fn previous_instr_pc(&self) -> usize {
        self.pc.saturating_sub(if self.is_in_thumb_mode() { 2 } else { 4 })
    }

    pub fn set_pc(&mut self, pc: usize) {
        // Function pointers to thumb functions have the lowest bit set,
        // while ARM ones don't. We need to mask off this bit in the PC,
        // because thumb mode is controlled by CPSR, and PC needs to
        // remain aligned to at least 2 bytes.
        self.pc = pc & !1;
        if pc & 1 == 0 {
            self.psr &= !(1 << 5)
        } else {
            self.psr |= 1 << 5;
        }
    }

    pub fn processor_mode(&self) -> ProcessorMode { ProcessorMode::from_psr(self.psr) }

    pub fn set_processor_mode(&mut self, mode: ProcessorMode) {
        self.psr &= !PROCESSOR_MODE_MASK;
        self.psr |= mode.to_usize().unwrap();
    }

    pub fn disable_interrupts(&mut self) {
        // Set Mask IRQ and Mask FIQ bits
        self.psr |= 0x80 | 0x40;
    }

    pub fn get_args(&self) -> [usize; 8] {
        [self.r0, self.r1, self.r2, self.r3, self.r4, self.r5, self.r8, self.r9]
    }

    pub fn set_args(&mut self, args: [usize; 8]) {
        self.r0 = args[0];
        self.r1 = args[1];
        self.r2 = args[2];
        self.r3 = args[3];
        self.r4 = args[4];
        self.r5 = args[5];
        self.r8 = args[6];
        self.r9 = args[7];
    }

    pub fn init_process_params(&mut self) {
        self.r0 = 0;
        self.r1 = 0;
        self.r2 = crate::platform::rand::get_u32() as usize; // random stack canary
    }
}

impl Default for Thread {
    fn default() -> Self {
        Thread {
            r0: 0,
            r1: 0,
            r2: 0,
            r3: 0,
            r4: 0,
            r5: 0,
            r6: 0,
            r7: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            fp: 0,
            ip: 0,
            sp: 0,
            lr: 0,
            pc: 0,
            psr: 0,
            tp: 0,
            fpscr: 0,
            s0_s31: [0; 32],
            s32_s63: [0; 32],
            stack: None,
            _padding: [0; 43],
        }
    }
}

impl core::fmt::Debug for Thread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut spsr_decoded = [0u8; 48];
        let psr_str_len = decode_psr(self.psr, &mut spsr_decoded);
        let psr_str = core::str::from_utf8(&spsr_decoded[..psr_str_len]).expect("decoded spsr str");

        writeln!(f, "\tPC:    {:08x}   SP:    {:08x}    TP: {:08x}", self.pc, self.sp, self.tp,)?;
        if let Some(stack) = self.stack {
            writeln!(
                f,
                "\tStack:    {:08x}-{:08x}",
                stack.as_ptr() as usize,
                stack.as_ptr() as usize + stack.len()
            )?;
        }
        writeln!(
            f,
            "\tR0:    {:08x}   R1:    {:08x}    R2: {:08x}    R3: {:08x}",
            self.r0, self.r1, self.r2, self.r3,
        )?;
        writeln!(
            f,
            "\tR4:    {:08x}   R5:    {:08x}    R6: {:08x}    R7: {:08x}",
            self.r4, self.r5, self.r6, self.r7,
        )?;
        writeln!(
            f,
            "\tR8:    {:08x}   R9:    {:08x}   R10: {:08x}   R11: {:08x}",
            self.r8, self.r9, self.r10, self.fp,
        )?;
        writeln!(
            f,
            "\tIP:    {:08x}   LR:    {:08x}  SPSR: {:08x} | {}",
            self.ip, self.lr, self.psr, psr_str,
        )?;
        if self.fpscr != 0 {
            writeln!(f, "\t [ VFP/NEON context ]")?;
            writeln!(f, "\tFPSCR: {:08x}", self.fpscr,)?;
            for (i, s) in self.s0_s31.chunks_exact(4).enumerate() {
                write!(f, "\t")?;
                for (si, sx) in s.iter().enumerate() {
                    write!(f, "S{:02}:   {:08x}  ", i * 4 + si, sx)?;
                }
                writeln!(f)?;
            }
            for (i, s) in self.s32_s63.chunks_exact(4).enumerate().map(|(i, s)| (i + 8, s)) {
                write!(f, "\t")?;
                for (si, sx) in s.iter().enumerate() {
                    write!(f, "S{:02}:   {:08x}  ", i * 4 + si, sx)?;
                }
                writeln!(f)?;
            }
        } else {
            writeln!(f, "\t [ no VFP/NEON context ]")?;
        }
        Ok(())
    }
}

// See ARM ARM
// B1.3.1 ARM processor modes
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive, ToPrimitive)]
#[allow(clippy::upper_case_acronyms)]
pub enum ProcessorMode {
    User = 0b10000,
    FIQ = 0b10001,
    IRQ = 0b10010,
    Service = 0b10011,
    Monitor = 0b10110,
    Abort = 0b10111,
    Undefined = 0b11011,
    System = 0b11111,
    Invalid = 0,
}

impl ProcessorMode {
    fn from_psr(psr: usize) -> Self { Self::from_usize(psr & PROCESSOR_MODE_MASK).unwrap_or(Self::Invalid) }

    fn to_short_str(&self) -> &'static [u8] {
        match self {
            ProcessorMode::User => b"USR",
            ProcessorMode::FIQ => b"FIQ",
            ProcessorMode::IRQ => b"IRQ",
            ProcessorMode::Service => b"SVC",
            ProcessorMode::Monitor => b"MON",
            ProcessorMode::Abort => b"ABT",
            ProcessorMode::Undefined => b"UND",
            ProcessorMode::System => b"SYS",
            ProcessorMode::Invalid => b"???",
        }
    }
}

/// Decodes the CPSR/SPSR register according to the ARM ARM "B1.3.3 Program Status Registers (PSRs)"
/// into a human-readable string of flags + mode.
fn decode_psr(psr: usize, spsr_str: &mut [u8]) -> usize {
    let bits = [
        (31, 'N'),
        (30, 'Z'),
        (29, 'C'),
        (28, 'V'),
        (27, 'Q'),
        (24, 'J'),
        (9, 'E'),
        (8, 'A'),
        (7, 'I'),
        (6, 'F'),
        (5, 'T'),
    ];
    let mut curr = 0;
    for (bit, ch) in bits.iter() {
        if (psr >> *bit) & 1 != 0 {
            spsr_str[curr] = *ch as u8;
            spsr_str[curr + 1] = b',';
            spsr_str[curr + 2] = b' ';
            curr += 3;
        }
    }

    if curr != 0 {
        curr -= 2;
        spsr_str[curr] = b' ';
        curr += 1;
    }

    // Add mode field
    spsr_str[curr..curr + 3].copy_from_slice(ProcessorMode::from_psr(psr).to_short_str());

    curr + 3
}
