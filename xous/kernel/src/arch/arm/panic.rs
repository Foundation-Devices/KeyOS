// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use core::fmt::Write;
use core::panic::PanicInfo;

use atsama5d27::rstc::Rstc;
use keyos::{RSTC_KERNEL_ADDR, SECURAM_KERNEL_ADDR};
use securam_manager::{KernelPanicMessage, SecuramManager};

use crate::debug::BufStr;

const PANIC_MAX_LINE_LENGTH: usize = 26;

#[panic_handler]
fn handle_panic(_arg: &PanicInfo) -> ! {
    let panic_message_buf = crate::SystemServices::with_mut(|ss| {
        let mut panic_message_buf = BufStr::<[u8; KernelPanicMessage::MAX_MSG_LENGTH - 1]>::new();

        write!(panic_message_buf, "PANIC (PID {}):\n{}", crate::arch::process::current_pid(), _arg).ok();

        // Check if there's an existing user process panic message with a backtrace
        let (existing_pid, existing_msg) = ss.get_panic_message();
        let mut has_user_backtrace = false;
        // Include the existing user process panic message if present
        if existing_pid == Some(crate::arch::process::current_pid()) {
            if let Ok(msg) = core::str::from_utf8(existing_msg) {
                write!(panic_message_buf, "\n{}", msg).ok();
            }
            has_user_backtrace = existing_msg.windows(10).any(|w| w == b"Backtrace:");
        }

        // Only capture kernel backtrace if we don't already have a user process backtrace
        if !has_user_backtrace {
            let backtrace = crate::arch::backtrace::Backtrace::capture();
            if backtrace.depth() > 0 {
                write!(panic_message_buf, "\nBacktrace:").ok();
                for (i, addr) in backtrace.iter().enumerate() {
                    if i % 3 == 0 {
                        write!(panic_message_buf, "\n ").ok();
                    }
                    write!(panic_message_buf, " {:07x}", addr).ok();
                }
            }
        }

        panic_message_buf
    });

    println!("{panic_message_buf}");

    let mut panic_message_buf_wrapped = BufStr::<[u8; KernelPanicMessage::MAX_MSG_LENGTH - 1]>::new();
    let mut run_length = 0;
    for char in panic_message_buf.as_slice() {
        if *char == b'\n' {
            run_length = 0;
        } else if run_length >= PANIC_MAX_LINE_LENGTH {
            writeln!(panic_message_buf_wrapped).ok();
            run_length = 0;
        }

        write!(panic_message_buf_wrapped, "{}", *char as char).ok();
        run_length += 1;
    }

    match unsafe { SecuramManager::new(SECURAM_KERNEL_ADDR as _) } {
        Ok(mut securam_manager) => {
            securam_manager.set_kernel_panic_message(&panic_message_buf_wrapped.as_slice().into()).ok();
        }
        Err(_e) => println!("[!] SECURAM is invalid: {_e:?}"),
    }

    // Reset the device
    let rstc = Rstc::with_alt_base_addr(RSTC_KERNEL_ADDR as u32);
    rstc.do_reset();

    unreachable!()
}
