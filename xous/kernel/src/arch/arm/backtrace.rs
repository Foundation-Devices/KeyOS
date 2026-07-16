// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Frame pointer-based backtrace capture for crashed processes and kernel panics

use core::fmt::Write;
use core::ops::Range;

use keyos::{ASLR_END, ASLR_START, KERNEL_LOAD_OFFSET, PAGE_SIZE};
use xous::MemoryRange;

use crate::arch::process::current_pid;
use crate::arch::Thread;
use crate::mem::MemoryManager;
use crate::services::ArchProcess;

const MAX_FRAMES: usize = 32;

/// Raw instruction pointers that can be symbolicated offline using `addr2line`
#[derive(Clone)]
pub struct Backtrace {
    frames: [usize; MAX_FRAMES],
    depth: usize,
}

impl Backtrace {
    /// Capture backtrace from the process context
    pub fn capture_from_context(
        pc: usize,
        lr: usize,
        fp: usize,
        stack_bounds: Option<Range<usize>>,
        thumb_mode: bool,
        allow_kernel_addrs: bool,
    ) -> Self {
        let mut frames = [0usize; MAX_FRAMES];
        let mut depth = 0;
        if pc != 0 {
            frames[0] = pc;
            depth = 1;
        }

        // Include LR only if it looks like a valid code address
        if depth < MAX_FRAMES && is_valid_code_addr(lr, allow_kernel_addrs) {
            frames[depth] = lr;
            depth += 1;
        }

        let bounds = match stack_bounds {
            Some(b) if !b.is_empty() => b,
            _ => return Backtrace { frames, depth },
        };

        let mut current_fp = fp;
        let mut last_fp = 0usize;

        while depth < MAX_FRAMES && current_fp != 0 {
            if current_fp & 0x3 != 0 {
                break; // Must be 4-byte aligned
            }

            let read_range = if thumb_mode {
                current_fp..current_fp.saturating_add(8)
            } else {
                current_fp.saturating_sub(4)..current_fp.saturating_add(4)
            };

            if !bounds.contains(&read_range.start) || read_range.end > bounds.end {
                break;
            }
            if !is_range_accessible(read_range.clone(), allow_kernel_addrs) {
                break;
            }

            if last_fp != 0 && current_fp <= last_fp {
                break; // FP must increase (stack grows down)
            }
            last_fp = current_fp;

            unsafe {
                let (saved_fp, saved_lr) = if thumb_mode {
                    // Thumb: [FP+0]=saved_r7, [FP+4]=saved_lr
                    let fp_val = *(current_fp as *const usize);
                    let lr_val = *((current_fp as *const usize).add(1));
                    (fp_val, lr_val)
                } else {
                    // ARM: [FP+0]=saved_fp, [FP-4]=saved_lr
                    let fp_val = *(current_fp as *const usize);
                    let lr_val = *((current_fp as *const usize).sub(1));
                    (fp_val, lr_val)
                };

                frames[depth] = saved_lr;
                depth += 1;
                current_fp = saved_fp;
            }
        }

        Backtrace { frames, depth }
    }

    /// Capture backtrace from the current execution context (for kernel panics)
    #[inline(never)]
    pub fn capture() -> Self {
        let lr: usize;
        let fp: usize;
        let sp: usize;

        unsafe {
            core::arch::asm!(
                "mov {lr}, lr",
                "mov {fp}, r7",
                "mov {sp}, sp",
                lr = out(reg) lr,
                fp = out(reg) fp,
                sp = out(reg) sp,
            );
        }

        // Use conservative estimate from current SP
        let stack_bounds = sp..sp.saturating_add(64 * 1024);
        Self::capture_from_context(lr, 0, fp, Some(stack_bounds), true, true)
    }

    pub fn depth(&self) -> usize { self.depth }

    pub fn iter(&self) -> impl Iterator<Item = &usize> { self.frames[..self.depth].iter() }
}

fn is_valid_code_addr(addr: usize, allow_kernel_addrs: bool) -> bool {
    if addr >= KERNEL_LOAD_OFFSET {
        return allow_kernel_addrs;
    }

    if !(ASLR_START..ASLR_END).contains(&addr) {
        return false;
    }

    let page_addr = addr & !(PAGE_SIZE - 1);
    crate::arch::mem::MemoryMapping::current().address_executable(page_addr as *const usize)
}

fn is_range_accessible(range: Range<usize>, allow_kernel_addrs: bool) -> bool {
    if range.start >= KERNEL_LOAD_OFFSET {
        return allow_kernel_addrs;
    }

    // For user addresses, use check_range_accessible
    let Ok(mem_range) = (unsafe { MemoryRange::new(range.start, range.len()) }) else {
        return false;
    };
    MemoryManager::with(|mm| mm.check_range_accessible(mem_range).is_ok())
}

/// Prints a backtrace for the current (crashing) process
pub fn capture_current_process_backtrace() {
    let aslr_slide =
        crate::SystemServices::with(|ss| ss.process(current_pid()).map(|p| p.aslr_slide).unwrap_or(0));

    ArchProcess::with_current(|process| {
        capture_backtrace_from_thread(process.current_thread(), aslr_slide);
    });
}

fn capture_backtrace_from_thread(thread: &Thread, aslr_slide: usize) {
    let is_thumb = thread.is_in_thumb_mode();
    let frame_pointer = if is_thumb { thread.r7 } else { thread.fp };

    let Some(stack) = thread.stack else {
        append_to_panic_message(b"\nBacktrace: <stack bounds unknown>");
        return;
    };

    let stack_bounds = (stack.as_ptr() as usize)..(stack.as_ptr() as usize + stack.len());

    if frame_pointer == 0 || !stack_bounds.contains(&frame_pointer) {
        let pc_file = thread.pc.wrapping_sub(aslr_slide);
        let lr_file = thread.lr.wrapping_sub(aslr_slide);
        let mut buf = [0u8; 64];
        let mut writer = crate::debug::BufStr::from(&mut buf[..]);
        let _ = write!(writer, "\nBacktrace:\n  {:07x} {:07x}", pc_file, lr_file);
        let msg = writer.as_slice();
        append_to_panic_message(msg);
        return;
    }

    let backtrace = Backtrace::capture_from_context(
        thread.pc,
        thread.lr,
        frame_pointer,
        Some(stack_bounds),
        is_thumb,
        false, // kernel addresses are not allowed in backtraces
    );

    append_backtrace(&backtrace, aslr_slide);
}

fn append_backtrace(backtrace: &Backtrace, aslr_slide: usize) {
    if backtrace.depth() == 0 {
        append_to_panic_message(b"\nBacktrace: <empty>");
        return;
    }

    let mut buf = [0u8; 512];
    let mut writer = crate::debug::BufStr::from(&mut buf[..]);
    let _ = write!(writer, "\nBacktrace:");

    for (i, addr) in backtrace.iter().enumerate() {
        if i % 3 == 0 {
            let _ = write!(writer, "\n ");
        }
        if *addr == crate::arch::process::EXIT_THREAD {
            let _ = write!(writer, " [exit]");
        } else {
            let _ = write!(writer, " {:07x}", addr.wrapping_sub(aslr_slide));
        }
    }

    let msg = writer.as_slice();
    append_to_panic_message(msg);
}

fn append_to_panic_message(msg: &[u8]) {
    crate::SystemServices::with_mut(|ss| {
        ss.append_panic_message(msg).ok();
    });
}
