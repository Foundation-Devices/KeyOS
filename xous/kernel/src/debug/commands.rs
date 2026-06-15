// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use core::fmt::Write;

use keyos::{PLAINTEXT_DRAM_BASE, PLAINTEXT_DRAM_END};

#[cfg(any(not(feature = "production"), feature = "log-serial"))]
use crate::process::current_pid;
use crate::{debug::BufStr, services::MAX_THREAD_COUNT};

pub fn debug_command(cmd: u8, mut output: impl core::fmt::Write) {
    // 16 KB temporary buffer in kernel space that can be used to print
    // when another process is activated. It is static instead of a stack
    // variable, because the kernel has a tiny allocated stack.
    #[cfg(any(not(feature = "production"), feature = "log-serial"))]
    static mut TMP: [u8; 0x10000] = [0; 0x10000];
    match cmd {
        #[cfg(any(not(feature = "production"), feature = "log-serial"))]
        b'i' => {
            writeln!(output, "Interrupt handlers:").ok();
            writeln!(output, "  IRQ | Process | Handler | Argument").ok();
            crate::services::SystemServices::with(|system_services| {
                crate::irq::for_user_each_irq(|irq, pid, address, arg| {
                    writeln!(
                        output,
                        "    {irq}:  {} @ {address:x?} {arg:x?}",
                        system_services.process(*pid).unwrap().name().unwrap_or(""),
                    )
                    .ok();
                });
                crate::irq::for_kernel_each_irq(|irq| {
                    writeln!(output, "    {irq}:  KERNEL").ok();
                });
            });
        }
        #[cfg(any(not(feature = "production"), feature = "log-serial"))]
        b'm' => {
            writeln!(output, "Printing memory page tables").ok();
            crate::services::SystemServices::with(|system_services| {
                let current_pid = current_pid();
                for process in system_services.processes.iter().flatten() {
                    writeln!(output, "PID {} {}:", process.pid, process.name().unwrap_or("")).ok();
                    let mut buffer = BufStr::from(unsafe { (*core::ptr::addr_of_mut!(TMP)).as_mut_slice() });
                    process.activate();
                    crate::arch::mem::MemoryMapping::current().print_map(&mut buffer);
                    system_services.process(current_pid).unwrap().activate();
                    writeln!(output, "{buffer}").ok();
                }
            });
        }
        b'p' => print_processes(&mut output),
        b't' => super::process_list::print_processes_compact(&mut output),
        #[cfg(any(not(feature = "production"), feature = "log-serial"))]
        b'P' => {
            writeln!(output, "Printing processes and threads").ok();
            crate::services::SystemServices::with(|system_services| {
                let current_pid = current_pid();
                for process in system_services.processes.iter().flatten() {
                    let mut buffer = BufStr::from(unsafe { (*core::ptr::addr_of_mut!(TMP)).as_mut_slice() });
                    process.activate();
                    system_services.print_current_process(&mut buffer).unwrap();
                    system_services.process(current_pid).unwrap().activate();
                    writeln!(output, "{buffer}").ok();
                }
            });
        }
        #[cfg(any(not(feature = "production"), feature = "log-serial"))]
        b'o' => {
            writeln!(output, "RAM ownership stats:").ok();
            crate::mem::MemoryManager::with(|mm| {
                mm.print_ownership(&mut output);
            });
        }
        #[cfg(any(not(feature = "production"), feature = "log-serial"))]
        b's' => {
            writeln!(output, "Servers in use:").ok();
            crate::services::SystemServices::with(|system_services| {
                writeln!(output, " idx | pid | process              | sid").ok();
                writeln!(output, " --- + --- + -------------------- + ------------------").ok();
                for (idx, server) in system_services.servers.iter().enumerate() {
                    if let Some(s) = server {
                        writeln!(
                            output,
                            " {:3} | {:3} | {:20} | {:x?}",
                            idx,
                            s.pid,
                            system_services.process(s.pid).unwrap().name().unwrap_or(""),
                            s.sid
                        )
                        .ok();
                    }
                }
            });
        }
        #[cfg(any(not(feature = "production"), feature = "log-serial"))]
        b'c' => {
            crate::platform::atsama5d2::cache::print_l2cache_stats();
        }
        b'a' => print_app_ids(&mut output),
        #[cfg(any(not(feature = "production"), feature = "log-serial"))]
        b'k' => {
            writeln!(output, "Page table and ownership consistency check").ok();
            crate::services::SystemServices::with(|system_services| {
                let current_pid = current_pid();
                for process in system_services.processes.iter().flatten() {
                    let mut buffer = BufStr::from(unsafe { (*core::ptr::addr_of_mut!(TMP)).as_mut_slice() });
                    writeln!(output, "PID {} {}:", process.pid, process.name().unwrap_or("")).ok();
                    process.activate();
                    crate::mem::MemoryManager::with(|mm| {
                        crate::arch::mem::MemoryMapping::current().check_consistency(mm, &mut buffer);
                    });
                    system_services.process(current_pid).unwrap().activate();
                    write!(output, "{buffer}").ok();
                    writeln!(output, "Consistency check finished").ok();
                    writeln!(output).ok();
                }
            });
        }
        #[cfg(any(not(feature = "production"), feature = "log-serial"))]
        b'h' => print_help(output),
        _ => {}
    }
}

fn print_processes(mut output: impl core::fmt::Write) {
    writeln!(
        output,
        " pid | ppid | process                          | state   | CPU  | RAM      | connections"
    )
    .ok();
    writeln!(
        output,
        " --- + ---- + -------------------------------- + ------- + ---- + -------- + -----------"
    )
    .ok();

    let totals = super::process_list::for_each_process_row(|row| {
        let mut state = BufStr::<[u8; MAX_THREAD_COUNT]>::new();
        for ch in row.thread_states {
            state.write_char(ch).ok();
        }

        writeln!(
            output,
            " {:3} | {:4} | {:<32} | {state} | {:3}% | {:6} K | {:11}",
            row.pid,
            row.ppid,
            row.name,
            row.cpu_percent,
            row.ram_used / 1024,
            row.connection_count,
        )
        .ok();
    });

    writeln!(
        output,
        " --- + ---- + -------------------------------- + ------- + ---- + -------- + -----------"
    )
    .ok();

    let cpu_used_percent = (totals.total_cpu_usage - totals.cpu_idle) * 100 / totals.total_cpu_usage;
    write!(output, "CPU Usage:").ok();
    print_progress_bar(cpu_used_percent as usize, &mut output);
    writeln!(output, "{}%", cpu_used_percent).ok();

    let total_ram_size = PLAINTEXT_DRAM_END - PLAINTEXT_DRAM_BASE;
    let ram_used_percent = totals.total_ram_usage as u64 * 100 / total_ram_size as u64;

    write!(output, "RAM Usage:").ok();
    print_progress_bar(ram_used_percent as usize, &mut output);
    writeln!(
        output,
        "{}% ({}K / {}K)",
        ram_used_percent,
        totals.total_ram_usage / 1024,
        total_ram_size / 1024
    )
    .ok();
}

fn print_app_ids(mut output: impl core::fmt::Write) {
    writeln!(output, " pid | process                          | AppId").ok();
    writeln!(output, " --- + -------------------------------- | --------------------------------").ok();
    crate::services::SystemServices::with(|system_services| {
        for process in system_services.processes.iter() {
            let Some(process) = process else { continue };
            write!(output, " {:3} | {:32} | ", process.pid, process.name().unwrap_or("N/A")).ok();
            for i in process.app_id().0 {
                write!(output, "{:02x}", i).ok();
            }
            writeln!(output).ok();
        }
    });
    writeln!(output, " --- + -------------------------------- | --------------------------------").ok();
    writeln!(output, " pid | process                          | AppId").ok();
}

fn print_progress_bar(pct: usize, mut output: impl core::fmt::Write) {
    const SKIP_STEPS: usize = 3;

    write!(output, "[").ok();
    for _ in (0..pct).filter(|i| i % SKIP_STEPS == 0) {
        write!(output, "=").ok();
    }
    for _ in (0..100_usize.saturating_sub(pct)).filter(|i| i % SKIP_STEPS == 0) {
        write!(output, " ").ok();
    }
    write!(output, "] ").ok();
}

#[cfg(any(not(feature = "production"), feature = "log-serial"))]
pub fn print_help(mut output: impl core::fmt::Write) {
    writeln!(output, "KeyOS Kernel Debug").ok();
    writeln!(output, "key | command").ok();
    writeln!(output, "--- + -----------------------").ok();
    writeln!(output, " h  | print this message").ok();
    writeln!(output, " i  | print irq handlers").ok();
    writeln!(output, " m  | print MMU page tables of all processes").ok();
    writeln!(output, " p  | print all processes").ok();
    writeln!(output, " t  | print all processes (compact)").ok();
    writeln!(output, " P  | print all processes and threads").ok();
    writeln!(output, " s  | print all allocated servers").ok();
    writeln!(output, " c  | print L2 cache stats").ok();
    writeln!(output, " a  | print App IDs").ok();
    writeln!(output, " o  | print Memory ownership").ok();
    writeln!(output, " k  | check page table and ownership consistency").ok();
}
