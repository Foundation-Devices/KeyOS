// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use core::fmt::Write;

use keyos::{PLAINTEXT_DRAM_BASE, PLAINTEXT_DRAM_END};

use crate::process::{current_pid, ThreadState};
use crate::services::{MAX_PROCESS_COUNT, MAX_THREAD_COUNT};

pub(crate) struct ProcessListRow<'a> {
    pub(crate) pid: usize,
    pub(crate) ppid: usize,
    pub(crate) name: &'a str,
    pub(crate) thread_states: [char; MAX_THREAD_COUNT],
    pub(crate) cpu_percent: u64,
    pub(crate) ram_used: usize,
    pub(crate) connection_count: usize,
}

pub(crate) struct ProcessListTotals {
    pub(crate) total_ram_usage: usize,
    pub(crate) cpu_idle: u64,
    pub(crate) total_cpu_usage: u64,
}

pub(crate) fn print_processes_compact(mut output: impl Write) {
    let process_count = crate::services::SystemServices::with(|system_services| {
        system_services.processes.iter().flatten().count()
    });
    writeln!(output, "PROC {}", process_count).ok();

    let totals = for_each_process_row(|row| {
        write!(output, "R {} {} {} ", row.pid, row.ppid, row.name).ok();
        let len = row.thread_states.iter().rposition(|ch| *ch != ' ').map(|idx| idx + 1).unwrap_or(1);
        for ch in &row.thread_states[..len] {
            let out = if *ch == ' ' { '.' } else { *ch };
            write!(output, "{}", out).ok();
        }
        writeln!(output, " {} {} {}", row.cpu_percent, row.ram_used / 1024, row.connection_count).ok();
    });

    let cpu_used_percent = (totals.total_cpu_usage - totals.cpu_idle) * 100 / totals.total_cpu_usage;
    let total_ram_size = PLAINTEXT_DRAM_END - PLAINTEXT_DRAM_BASE;
    writeln!(output, "SUM {} {} {}", cpu_used_percent, totals.total_ram_usage / 1024, total_ram_size / 1024)
        .ok();
}

pub(crate) fn for_each_process_row(mut on_row: impl FnMut(ProcessListRow<'_>)) -> ProcessListTotals {
    let mut totals = ProcessListTotals { total_ram_usage: 0, cpu_idle: 0, total_cpu_usage: 0 };

    crate::services::SystemServices::with(|system_services| {
        let mut cpu_usage_map = [0u64; MAX_PROCESS_COUNT];
        crate::scheduler::Scheduler::with(|scheduler| {
            for (pid, usage) in &scheduler.cpu_usage {
                cpu_usage_map[*pid as usize] += *usage as u64;
                totals.total_cpu_usage += *usage as u64;
            }
        });
        totals.cpu_idle = cpu_usage_map[1];
        cpu_usage_map[1] = 0;
        if totals.total_cpu_usage == 0 {
            totals.total_cpu_usage = 1;
        }

        let current_pid = current_pid();
        for process in system_services.processes.iter().flatten() {
            process.activate();

            let mut thread_states = [' '; MAX_THREAD_COUNT];
            for (tid, state) in thread_states.iter_mut().enumerate() {
                *state = match process.thread_state(tid) {
                    ThreadState::Free => ' ',
                    ThreadState::Ready => 'R',
                    ThreadState::WaitJoin { .. } => 'j',
                    ThreadState::RetryConnect { .. } => 'c',
                    ThreadState::RetryQueueFull { .. } => 'q',
                    ThreadState::WaitBlocking { .. } => 'b',
                    ThreadState::WaitReceive { .. } => 'w',
                    ThreadState::WaitFutex { .. } => 'f',
                    ThreadState::WaitMapZero => 'z',
                    ThreadState::RetryMapZero => 'Z',
                };
            }

            let ram_used = crate::mem::MemoryManager::with(|mm| mm.ram_used_by(process.pid));
            totals.total_ram_usage += ram_used;

            let row = ProcessListRow {
                pid: process.pid.get() as usize,
                ppid: process.ppid.map(|p| p.get() as usize).unwrap_or(0),
                name: process.name().unwrap_or("N/A"),
                thread_states,
                cpu_percent: cpu_usage_map[process.pid.get() as usize] * 100 / totals.total_cpu_usage,
                ram_used,
                connection_count: process.number_of_connections(),
            };

            system_services.process(current_pid).unwrap().activate();
            on_row(row);
        }
    });

    totals
}
