// SPDX-FileCopyrightText: 2022 Foundation Devices, Inc <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use core::arch::asm;

use keyos::{PAGE_SIZE, STACK_PAGE_COUNT};
use xous::{arch::irq::IrqNumber, MemoryRange, SysCall};

use crate::{
    arch::{
        process::{crash_current_process, current_pid, ProcessorMode, EXIT_THREAD},
        Thread,
    },
    mem::MemoryManager,
    services::ArchProcess,
    SystemServices,
};

/// Formats how far `addr` is from `stack`, suppressing output beyond
/// half a default stack size from either edge.
struct StackDistance {
    addr: usize,
    stack: Option<MemoryRange>,
}

impl core::fmt::Display for StackDistance {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Some(stack) = self.stack else { return Ok(()) };
        let start = stack.as_ptr() as usize;
        let end = start + stack.len();
        let threshold = STACK_PAGE_COUNT * PAGE_SIZE / 2;
        let addr = self.addr;
        if addr >= start && addr < end {
            write!(f, " (inside thread stack)")
        } else if addr < start {
            let dist = start - addr;
            if dist <= threshold {
                write!(f, " (0x{dist:x} bytes below stack)")
            } else {
                Ok(())
            }
        } else {
            let dist = addr - end + 1;
            if dist <= threshold {
                write!(f, " (0x{dist:x} bytes above stack)")
            } else {
                Ok(())
            }
        }
    }
}

extern "Rust" {
    fn _xous_syscall_return_result(context: &Thread, enable_irqs: bool) -> !;
}

pub fn enable_irq(irq_no: IrqNumber) {
    klog!("Enabling IRQ {:?}", irq_no);

    #[cfg(feature = "trace-systemview")]
    {
        SystemServices::with(|ss| {
            let pid = current_pid();
            let mut name_buf = [0u8; 32];

            let suffix = b" (ISR)";
            let name = ss.process(pid).expect("process").name().expect("process name");
            let len = name.len();
            let max_len = name_buf.len() - suffix.len();

            if len < max_len {
                name_buf[..name.len()].copy_from_slice(name.as_bytes());
                name_buf[len..len + suffix.len()].copy_from_slice(suffix);
            } else {
                name_buf[..max_len].copy_from_slice(name[..max_len].as_bytes());
                name_buf[max_len..].copy_from_slice(suffix);
            }

            let stack = unsafe { xous::MemoryRange::new(keyos::IRQ_STACK_BOTTOM, PAGE_SIZE).expect("stack") };
            let name = core::str::from_utf8(&name_buf).expect("process name");
            let irq_str = irq_number_to_str(irq_no);
            let irq_str = core::str::from_utf8(&irq_str).expect("irq str");
            systemview_keyos::SystemView::send_system_description(irq_str);
            systemview_keyos::SystemView::thread_send_info(pid, crate::process::IRQ_TID, name, stack);
        });
    }

    crate::platform::atsama5d2::aic::set_irq_enabled(irq_no, true);
}

pub fn disable_irq(irq_no: IrqNumber) {
    klog!("Disabling IRQ {:?}", irq_no);

    crate::platform::atsama5d2::aic::set_irq_enabled(irq_no, false);
}

#[export_name = "_swi_handler_rust"]
pub extern "C" fn swi_handler() {
    let (tid, [a0, a1, a2, a3, a4, a5, a6, a7]) = ArchProcess::with_current_mut(|p| {
        let tid = p.current_tid();
        let thread = p.current_thread();
        klog!("SWI at {}:{tid}", crate::arch::process::current_pid());
        let mode = p.current_thread().processor_mode();
        if mode != ProcessorMode::User && mode != ProcessorMode::System {
            let pid = crate::arch::process::current_pid();
            panic!("SWI in kernel mode ({mode:?})! pid: {pid}");
        }

        (tid, thread.get_args())
    });
    #[cfg(feature = "trace-systemview")]
    {
        let kernel_id = systemview_keyos::pid_tid_to_id(xous::PID::new(1).unwrap(), 2);
        systemview_keyos::SystemView::task_exec_begin(kernel_id);
    }

    klog!("Handling syscall | args = ({:08x?})", [a0, a1, a2, a3, a4, a5, a6, a7]);

    let call = SysCall::from_args(a0, a1, a2, a3, a4, a5, a6, a7).unwrap_or_else(|_e| {
        ArchProcess::with_current_mut(|p| {
            println!("[!] Invalid syscall {a0}: {_e:?}");
            xous::Result::Error(xous::Error::UnhandledSyscall).to_args();
            p.current_thread_mut().set_args(xous::Result::Error(xous::Error::UnhandledSyscall).to_args());
            resume(p.current_thread_mut());
        })
    });
    #[cfg(feature = "trace-systemview")]
    let call_args = {
        let call_args = call.as_args();
        systemview_keyos::SystemView::trace_syscall(&call_args);
        call_args
    };

    let response = crate::syscall::handle(tid, call).unwrap_or_else(xous::Result::Error);

    #[cfg(feature = "trace-systemview")]
    {
        let response_args = response.to_args();
        systemview_keyos::SystemView::trace_syscall_result(&call_args, response_args[0]);
    }

    klog!("Syscall Result: {:x?}", response);

    ArchProcess::with_current_mut(|p| {
        // If we're resuming a process that was previously sleeping, restore the
        // thread context. Otherwise, keep the thread context the same and pass
        // the return values in 8 argument registers.
        let tid = p.current_tid();
        if response != xous::Result::ResumeProcess {
            p.set_thread_result(tid, response);
        }

        let thread = p.current_thread_mut();
        klog!("Resuming {}:{}", current_pid(), tid);
        klog!("Returning to address {:08x}", thread.pc);

        #[cfg(feature = "trace-systemview")]
        {
            let id = systemview_keyos::pid_tid_to_id(current_pid(), tid);
            systemview_keyos::SystemView::task_exec_begin(id);
        }
        resume(thread);
    });
}

fn read_fault_cause() -> (usize, usize, usize, usize) {
    // Read fault status (DFSR, IFSR) and cause address (DFAR, IFAR) registers
    let mut dfar: usize;
    let mut ifar: usize;
    let mut dfsr: usize;
    let mut ifsr: usize;
    unsafe {
        asm!(
            "mrc p15, 0, {dfar}, c6, c0, 0",
            "mrc p15, 0, {ifar}, c6, c0, 2",
            "mrc p15, 0, {dfsr}, c5, c0, 0",
            "mrc p15, 0, {ifsr}, c5, c0, 1",
            dfar = out(reg) dfar,
            ifar = out(reg) ifar,
            dfsr = out(reg) dfsr,
            ifsr = out(reg) ifsr,
        );
    }

    (dfar, ifar, dfsr, ifsr)
}

fn clear_fault() {
    let zero = 0;
    unsafe {
        asm!(
            "mcr p15, 0, {dfar}, c6, c0, 0",
            "mcr p15, 0, {ifar}, c6, c0, 2",
            "mcr p15, 0, {dfsr}, c5, c0, 0",
            "mcr p15, 0, {ifsr}, c5, c0, 1",
            dfar = in(reg) zero,
            ifar = in(reg) zero,
            dfsr = in(reg) zero,
            ifsr = in(reg) zero,
        );
    }
}

#[export_name = "_abort_handler_rust"]
pub extern "C" fn abort_handler() {
    let (dfar, ifar, dfsr, ifsr) = read_fault_cause();

    // See ARM ARM Table B3-12 VMSAv7 DFSR encodings
    let dfsr_fault_cause = dfsr & 0b1111;
    let ifsr_fault_cause = ifsr & 0b1111;
    let is_write = (dfsr & (1 << 11)) != 0;
    let pid = current_pid();

    let mode = ArchProcess::with_current(|p| p.current_thread().processor_mode());
    if mode != ProcessorMode::User && mode != ProcessorMode::System {
        super::backtrace::capture_current_process_backtrace();
        #[cfg(not(feature = "production"))]
        SystemServices::with(|ss| {
            crate::debug::serial::with_output(|stream| {
                ss.print_current_process(stream).ok();
            })
        });
        // Both the stack and the register file are probably corrupted at this point
        panic!(
            "Abort in kernel mode {:?}! pid: {}, addrD {:08x} addrI: {:08x}, causeD: {:04b} causeI: {:04b}",
            mode, pid, dfar, ifar, dfsr_fault_cause, ifsr_fault_cause
        );
    }

    klog!(
        "KERNEL({}): ABORT | addrD {:08x} addrI: {:08x}, causeD: {:04b} causeI: {:04b}",
        pid,
        dfar,
        ifar,
        dfsr_fault_cause,
        ifsr_fault_cause,
    );

    #[cfg(all(feature = "trace-systemview", feature = "trace-aborts-systemview"))]
    {
        // Notify SystemView that we're exiting from ISR if the abort occurred inside it
        if unsafe { PREVIOUS_PAIR.is_some() } {
            systemview_keyos::SystemView::isr_exit();
        }

        crate::platform::atsama5d2::systemview::set_abort();
        systemview_keyos::SystemView::isr_enter();
    }

    clear_fault();

    match ifar {
        EXIT_THREAD => {
            let tid = ArchProcess::with_current(|process| process.current_tid());

            // This address indicates a thread has exited. Destroy the thread.
            // This activates another thread within this process.
            SystemServices::with_mut(|ss| ss.thread_exited(tid)).unwrap();

            #[cfg(all(feature = "trace-systemview", feature = "trace-aborts-systemview"))]
            {
                systemview_keyos::SystemView::isr_exit();
            }

            ArchProcess::with_current_mut(|p| resume(p.current_thread_mut()));
        }

        _ => {
            let error: &str = match dfsr_fault_cause {
                // Page-level translation fault: may be an on-demand page awaiting backing.
                0b0111 => {
                    let ensure = MemoryManager::with_mut(|mm| {
                        mm.ensure_page_exists((dfar & !(PAGE_SIZE - 1)) as *mut usize)
                    });
                    match ensure {
                        Ok(()) => {
                            klog!("Handed new page to process");

                            #[cfg(all(feature = "trace-systemview", feature = "trace-aborts-systemview"))]
                            {
                                systemview_keyos::SystemView::isr_exit();
                            }

                            ArchProcess::with_current_mut(|process| {
                                let thread = process.current_thread_mut();

                                // Retry the instruction that caused abort
                                thread.pc = thread.pc.saturating_sub(8);

                                resume(process.current_thread_mut())
                            });
                            return;
                        }
                        Err(xous::Error::OutOfMemory) => {
                            let page = dfar & !(PAGE_SIZE - 1);
                            crash_current_process!(
                                "Out of physical memory for PID {pid} | virt: 0x{page:08x}"
                            );
                            return;
                        }
                        Err(_) => "Invalid memory access (L2)",
                    }
                }
                // Section-level translation fault: no L2 table, so no on-demand possible.
                0b0101 => "Invalid memory access (L1)",
                0b0001 => "Unaligned memory access",
                0b1101 | 0b1111 => "Memory permission violation",
                _ => {
                    crash_current_process!(
                        "Unhandled abort fault: PID {pid} | addrD {dfar:08x} addrI: {ifar:08x}, causeD: {dfsr_fault_cause:04b} causeI: {ifsr_fault_cause:04b}"
                    );
                    return;
                }
            };

            let verb = if is_write { "write" } else { "read" };
            let stack = ArchProcess::with_current(|p| p.current_thread().stack);
            let stack_suffix = StackDistance { addr: dfar, stack };

            crash_current_process!(
                "{error}: PID {pid} attempted to {verb} address 0x{dfar:08x}{stack_suffix} (DFSR 0x{dfsr:08x})"
            );
        }
    }
}

#[export_name = "_irq_handler_rust"]
pub extern "C" fn _irq_handler_rust() {
    let pid = current_pid();

    klog!("Entered irq handler (preempted {})", pid);

    let mode = ArchProcess::with_current_mut(|p| {
        let thread = p.current_thread_mut();
        // SAMA5D2x Datasheet (AIC):
        // The link register must be decremented by four when it is saved if it is to be restored directly
        // into the program counter at the end of the interrupt.
        thread.pc = thread.pc.saturating_sub(4);
        p.current_thread().processor_mode()
    });
    // IRQ shall only be enabled during user-mode, because
    // some of the kernel functionality are very non-preeemptible
    if mode != ProcessorMode::User && mode != ProcessorMode::System {
        super::backtrace::capture_current_process_backtrace();
        #[cfg(not(feature = "production"))]
        SystemServices::with(|ss| {
            crate::debug::serial::with_output(|stream| {
                ss.print_current_process(stream).ok();
            })
        });
        // The register file is probably corrupted at this point
        panic!("[!] IRQ received in kernel mode ({:?})! pid: {}", mode, pid);
    }

    if let Some(irq_pending) = crate::platform::atsama5d2::aic::get_pending_irq() {
        klog!("Pending irq: {:?}", irq_pending);
        crate::irq::handle(irq_pending).expect("Couldn't handle IRQ");
    } else {
        klog!("IRQ handler called with no pending IRQs");
    }
    crate::platform::atsama5d2::aic::acknowledge_irq();
    ArchProcess::with_current_mut(|process| {
        #[cfg(feature = "trace-systemview")]
        {
            systemview_keyos::SystemView::isr_exit();
            systemview_keyos::SystemView::task_exec_begin(systemview_keyos::pid_tid_to_id(
                current_pid(),
                process.current_tid(),
            ));
        }

        resume(process.current_thread_mut())
    })
}

#[export_name = "_undef_handler_rust"]
pub extern "C" fn undef_handler() {
    let pid = current_pid();
    let pc = ArchProcess::with_current(|p| p.current_thread().pc.saturating_sub(4));
    crash_current_process!("PID {pid} issued an undefined or forbidden instruction at address 0x{pc:08x}");
}

extern "C" {
    #[allow(improper_ctypes)]
    fn _resume_trampoline(thread: &Thread) -> !;
}

pub(crate) fn resume(thread: &mut Thread) -> ! {
    // Restore thread stack and PC, pass resume arguments via r0-r4
    klog!(
        "resume ({}): setting sp={:08x}, pc={:08x}, lr={:08x}, mode={:?}",
        crate::arch::arm::process::current_pid(),
        thread.sp,
        thread.pc,
        thread.lr,
        thread.processor_mode(),
    );

    unsafe {
        _resume_trampoline(thread);
    }
}

#[cfg(feature = "trace-systemview")]
fn irq_number_to_str(irq_number: IrqNumber) -> [u8; 16] {
    *match irq_number {
        IrqNumber::PeriodicIntervalTimer => b"I#0=PIT\0        ",
        IrqNumber::Uart0 => b"I#1=UART0\0      ",
        IrqNumber::Uart1 => b"I#2=UART1\0      ",
        IrqNumber::Uart2 => b"I#3=UART2\0      ",
        IrqNumber::Uart3 => b"I#4=UART3\0      ",
        IrqNumber::Uart4 => b"I#5=UART4\0      ",
        IrqNumber::Pioa => b"I#6=PIOA\0       ",
        IrqNumber::Piob => b"I#7=PIOB\0       ",
        IrqNumber::Pioc => b"I#8=PIOC\0       ",
        IrqNumber::Piod => b"I#9=PIOD\0       ",
        IrqNumber::Isi => b"I#10=ISC\0       ",
        IrqNumber::Lcdc => b"I#11=LCDC\0      ",
        IrqNumber::Uhphs => b"I#12=UHPHS\0     ",
        IrqNumber::Udphs => b"I#13=UDPHS\0     ",
        IrqNumber::Tc0 => b"I#14=TC0\0       ",
        IrqNumber::Tc1 => b"I#15=TC1\0       ",
        IrqNumber::Sdmmc0 => b"I#16=SDMMC0\0    ",
        IrqNumber::Xdmac0 => b"I#17=XDMAC0\0    ",
        IrqNumber::Xdmac1 => b"I#18=XDMAC1\0    ",
        IrqNumber::Flexcom2 => b"I#19=FLEXCOM2\0  ",
        IrqNumber::Sys => b"I#20=SYS\0       ",
        IrqNumber::Secumod => b"I#21=SECUMOD\0   ",
        IrqNumber::Icm => b"I#22=ICM\0       ",
    }
}
