// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(keyos)]
use core::ptr::{addr_of, addr_of_mut};

use xous::{SysCallResult, ThreadPriority, PID, TID};

#[cfg(keyos)]
use crate::process::{current_pid, ArchProcess};
use crate::{
    process::{IRQ_TID, MAX_THREAD_COUNT},
    services::{SystemServices, MAX_PROCESS_COUNT},
};

#[cfg(keyos)]
const CPU_MEASUREMENT_COUNT: usize = 1024;

#[cfg(keyos)]
const PROCESS_TIMESLICE_MS: usize = 500;

const NUM_PRIORITIES: usize = ThreadPriority::Highest as usize + 1;
/// A big unifying struct containing all of the system state.
pub struct Scheduler {
    queue_heads: [Option<(PID, TID)>; NUM_PRIORITIES],
    links: [[Option<SchedulerLink>; MAX_THREAD_COUNT]; MAX_PROCESS_COUNT],
    highest_ready_priority: usize,
    in_irq_handler: bool,

    /// A list of the most recent CPU usage measurements
    #[cfg(keyos)]
    pub cpu_usage: [(u8, usize); CPU_MEASUREMENT_COUNT],

    /// Current pointer into `cpu_usage`
    #[cfg(keyos)]
    cpu_usage_index: usize,

    #[cfg(keyos)]
    currently_measuring: u8,
}

// Ready threads are arranged into a circular linked list, with each queue_head pointing into the circle, to
// the most recently activated thread.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerLink {
    next_pid: PID,
    next_tid: TID,
    prev_pid: PID,
    prev_tid: TID,
}

#[cfg(not(keyos))]
std::thread_local!(static SCHEDULER: core::cell::RefCell<Scheduler> = core::cell::RefCell::new(Scheduler {
    queue_heads: [const { None }; NUM_PRIORITIES],
    highest_ready_priority: 0,
    links: [[const { None }; MAX_THREAD_COUNT]; MAX_PROCESS_COUNT],
    in_irq_handler: false,
}));

#[cfg(keyos)]
#[no_mangle]
static mut SCHEDULER: Scheduler = Scheduler {
    queue_heads: [const { None }; NUM_PRIORITIES],
    highest_ready_priority: 0,
    links: [[const { None }; MAX_THREAD_COUNT]; MAX_PROCESS_COUNT],
    in_irq_handler: false,
    cpu_usage: [(0, 0); CPU_MEASUREMENT_COUNT],
    cpu_usage_index: 0,
    currently_measuring: 0,
};

impl Scheduler {
    /// Calls the provided function with the current inner process state.
    #[allow(dead_code)]
    pub fn with<F, R>(f: F) -> R
    where
        F: FnOnce(&Scheduler) -> R,
    {
        #[cfg(keyos)]
        unsafe {
            f(&*addr_of!(SCHEDULER))
        }
        #[cfg(not(keyos))]
        SCHEDULER.with(|s| f(&s.borrow()))
    }

    pub fn with_mut<F, R>(f: F) -> R
    where
        F: FnOnce(&mut Scheduler) -> R,
    {
        #[cfg(keyos)]
        unsafe {
            f(&mut *addr_of_mut!(SCHEDULER))
        }

        #[cfg(not(keyos))]
        SCHEDULER.with(|s| f(&mut s.borrow_mut()))
    }

    fn link_copy(&self, pid: PID, tid: TID) -> Option<SchedulerLink> {
        self.links[pid.get() as usize - 1][tid - 1]
    }

    fn link_mut(&mut self, pid: PID, tid: TID) -> &mut Option<SchedulerLink> {
        &mut self.links[pid.get() as usize - 1][tid - 1]
    }

    pub fn ready_thread(&mut self, pid: PID, tid: TID, priority: ThreadPriority) {
        // IRQ handlers are 'scheduled' out of band
        if tid == IRQ_TID {
            assert!(!self.in_irq_handler, "Multiple IRQ threads marked ready");
            self.in_irq_handler = true;
            return;
        }
        let priority = priority as usize;
        match self.queue_heads[priority] {
            None => {
                self.queue_heads[priority] = Some((pid, tid));
                if self.highest_ready_priority < priority {
                    self.highest_ready_priority = priority;
                }
                *self.link_mut(pid, tid) =
                    Some(SchedulerLink { next_pid: pid, next_tid: tid, prev_pid: pid, prev_tid: tid });
            }
            Some((head_pid, head_tid)) => {
                assert!(self.link_copy(pid, tid).is_none(), "Thread linked twice");
                let head_copy = self.link_copy(head_pid, head_tid).expect("Queue head was not linked in");
                *self.link_mut(head_pid, head_tid) = Some(SchedulerLink {
                    next_pid: head_copy.next_pid,
                    next_tid: head_copy.next_tid,
                    prev_pid: pid,
                    prev_tid: tid,
                });
                let prev = self
                    .link_mut(head_copy.prev_pid, head_copy.prev_tid)
                    .as_mut()
                    .expect("Last queue element was not linked in");
                prev.next_pid = pid;
                prev.next_tid = tid;
                *self.link_mut(pid, tid) = Some(SchedulerLink {
                    next_pid: head_pid,
                    next_tid: head_tid,
                    prev_pid: head_copy.prev_pid,
                    prev_tid: head_copy.prev_tid,
                });
            }
        }
    }

    pub fn park_thread(&mut self, pid: PID, tid: TID, priority: ThreadPriority) {
        if tid == IRQ_TID {
            assert!(self.in_irq_handler, "IRQ thread parked when not ready");
            self.in_irq_handler = false;
            return;
        }
        let priority = priority as usize;
        let link = core::mem::take(self.link_mut(pid, tid)).expect("To-be-parked thread was not linked in");
        if link.next_pid == pid && link.next_tid == tid {
            self.queue_heads[priority] = None;
            if priority == self.highest_ready_priority {
                self.highest_ready_priority = self
                    .queue_heads
                    .iter()
                    .take(priority)
                    .rposition(|h| h.is_some())
                    .expect("No threads ready (not even idle)");
            }
        } else {
            if self.queue_heads[priority] == Some((pid, tid)) {
                self.queue_heads[priority] = Some((link.next_pid, link.next_tid));
            }

            let prev = self
                .link_mut(link.prev_pid, link.prev_tid)
                .as_mut()
                .expect("Prev queue element was not linked in");
            prev.next_pid = link.next_pid;
            prev.next_tid = link.next_tid;

            let next = self
                .link_mut(link.next_pid, link.next_tid)
                .as_mut()
                .expect("Next queue element was not linked in");
            next.prev_pid = link.prev_pid;
            next.prev_tid = link.prev_tid;
        }
    }

    pub fn yield_thread(&mut self, pid: PID, tid: TID, priority: ThreadPriority) {
        let priority = priority as usize;
        if self.queue_heads[priority] == Some((pid, tid)) {
            let link = self.link_copy(pid, tid).expect("Yielded thread was not linked in");
            self.queue_heads[priority] = Some((link.next_pid, link.next_tid));
        } else {
            println!("[!] Yielded thread was not current");
        }
    }

    #[cfg(not(keyos))]
    pub fn activate_current(&mut self, _services: &mut SystemServices) -> SysCallResult {
        Ok(xous::Result::ResumeProcess)
    }

    /// Records `ticks` of runtime against the process being measured.
    #[cfg(keyos)]
    pub fn record_cpu_usage(&mut self, ticks: usize) {
        self.cpu_usage_index = (self.cpu_usage_index + 1) % self.cpu_usage.len();
        self.cpu_usage[self.cpu_usage_index] = (self.currently_measuring, ticks);
    }

    #[cfg(keyos)]
    pub fn activate_current(&mut self, services: &mut SystemServices) -> SysCallResult {
        if self.in_irq_handler {
            return Ok(xous::Result::ResumeProcess);
        }
        let current_pid = current_pid();
        let current_tid = ArchProcess::with_current(|p| p.current_tid());

        let (next_pid, next_tid) =
            self.queue_heads[self.highest_ready_priority].expect("Highest prio head was empty");

        if next_pid == current_pid && next_tid == current_tid {
            return Ok(xous::Result::ResumeProcess);
        }

        // set up the preemption interrupt if we are switching away. Keep in mind that preemption is
        // more of a safety feature, so the exact logic of this is not important.
        #[cfg(keyos)]
        {
            use crate::platform::{cancel_preemption, setup_preemption, start_measuring_idle};

            let usage = cancel_preemption();
            // Don't pollute the table with short slices, and also handle the glitch case
            // where we were so fast that the timer couldn't even reset in time
            // (it waits for the next valid clock edge before actually resetting to 0)
            if usage > 1 && usage != self.cpu_usage[self.cpu_usage_index].1 {
                self.record_cpu_usage(usage);
            }
            if next_pid.get() == 1 {
                start_measuring_idle();
            } else {
                setup_preemption(PROCESS_TIMESLICE_MS);
            }
            self.currently_measuring = next_pid.get();
        }
        services.process(next_pid).expect("Chosen process did not exist").activate();
        ArchProcess::current().set_tid(next_tid);
        Ok(xous::Result::ResumeProcess)
    }
}
