// SPDX-FileCopyrightText: 2022 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use atsama5d27::tc::{Tc, TimerChannel, TimerInput};
use keyos::TC0_KERNEL_ADDR;
use utralib::HW_TC0_BASE;
use xous::{arch::irq::IrqNumber, MemoryFlags};

use crate::{
    irq::interrupt_claim_kernel,
    mem::MemoryManager,
    process::{current_pid, ArchProcess},
    scheduler::Scheduler,
    services::SystemServices,
};

const SLOW_CLOCK_SPEED: usize = 32768;

/// Initialize Timer Counter 0 driver.
pub fn init() {
    MemoryManager::with_mut(|memory_manager| {
        memory_manager
            .map_range(
                HW_TC0_BASE,
                TC0_KERNEL_ADDR as *mut usize,
                0x1000,
                MemoryFlags::W | MemoryFlags::DEV,
                false,
            )
            .expect("unable to map TC0 to kernel")
    });
    interrupt_claim_kernel(IrqNumber::Tc0, tc0_interrupt_handler);
    let mut tc0 = Tc::with_alt_base_addr(TC0_KERNEL_ADDR as u32, TimerChannel::Ch0);
    tc0.setup(TimerInput::SlowClock);
}

pub fn set_timeout(ticks_ms: usize) {
    let tc0 = Tc::with_alt_base_addr(TC0_KERNEL_ADDR as u32, TimerChannel::Ch0);
    tc0.set_period((ticks_ms * SLOW_CLOCK_SPEED / 1000) as u32);
    tc0.set_interrupt(true);
    tc0.restart();
}

pub fn start_freerunning() {
    let tc0 = Tc::with_alt_base_addr(TC0_KERNEL_ADDR as u32, TimerChannel::Ch0);
    tc0.set_period(0xFFFFFFFF);
    tc0.set_interrupt(false);
    tc0.restart();
}

pub fn stop() -> usize {
    let tc0 = Tc::with_alt_base_addr(TC0_KERNEL_ADDR as u32, TimerChannel::Ch0);
    tc0.stop();
    // Reading the flag clears it. A compare latched during this kernel entry would otherwise
    // fire the handler after the switch, charging its slice to the next thread.
    tc0.period_passed();
    tc0.counter() as usize
}

fn tc0_interrupt_handler() {
    let tc0 = Tc::with_alt_base_addr(TC0_KERNEL_ADDR as u32, TimerChannel::Ch0);
    // Don't stop the timer: RC compare reloads it, and stopping would leave preemption off
    // until the next switch, which a lone runnable thread never reaches.
    // Reading the flag clears it, acknowledging the interrupt.
    tc0.period_passed();

    let tid = ArchProcess::with_current(|p| p.current_tid());
    SystemServices::with_mut(|ss| {
        let prio = ss.current_process().thread_priority(tid);
        Scheduler::with_mut(|s| {
            // The counter has just reset, so this slice is only measurable here.
            s.record_cpu_usage(tc0.period() as usize);
            s.yield_thread(current_pid(), tid, prio);
            s.activate_current(ss).ok();
        });
    })
}
