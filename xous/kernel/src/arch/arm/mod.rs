// SPDX-FileCopyrightText: 2022 Foundation Devices, Inc <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

pub mod asm;
pub mod backtrace;
pub mod elf;
pub mod irq;
pub mod mem;
pub mod panic;
pub mod process;

use core::arch::asm;
use core::num::NonZeroU8;

pub use process::Thread;
use xous::PID;

use crate::platform;

pub fn current_hw_pid() -> PID {
    let mut current_pid: usize;
    unsafe {
        asm!(
            "mrc p15, 0, {contextidr}, c13, c0, 1",
            contextidr = out(reg) current_pid,
        );

        assert_ne!(current_pid, 0, "Hardware PID is zero");

        NonZeroU8::new_unchecked((current_pid & 0xff) as u8)
    }
}
pub fn init() {
    // The boot thread is never dispatched via set_tid, so seed it here, or
    // else PID1's first trap saves registers through a garbage TPIDRPRW.
    process::Process::current().set_tid(crate::process::INITIAL_TID);

    unsafe {
        let pid = 1;
        let contextidr = (pid << 8) | pid;
        // Set initial (kernel) CONTEXTIDR
        asm!(
            "mcr p15, 0, {contextidr}, c13, c0, 1",
            contextidr = in(reg) contextidr,
        );

        // Clean the VFP/NEON state
        let zeros: [u32; 32] = [0; 32];
        asm!(
            "vldm  {}, {{d0-d15}}",
            "vldm  {}, {{d16-d31}}",
            in(reg) &zeros,
            in(reg) &zeros,
        );
    }
}

pub fn idle() -> bool { platform::atsama5d2::idle::idle() }
