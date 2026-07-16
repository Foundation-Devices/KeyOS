// SPDX-FileCopyrightText: 2022 Foundation Devices, Inc <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use keyos::{DDRC_KERNEL_ADDR, IDLE_FUNCTION_MEM_SIZE, IDLE_FUNCTION_PHYS_ADDR, PMC_KERNEL_ADDR};
use utralib::HW_MPDDRC_BASE;
use xous::{DramIdleMode, MemoryFlags};

use super::cache::{clean_cache_l2, invalidate_instruction_cache};
use crate::{
    mem::MemoryManager,
    platform::atsama5d2::cache::{clean_cache_l1, disable_l2_cache, enable_l2_cache},
};

static mut SUSPEND_FN: Option<unsafe extern "C" fn(usize, usize)> = None;

static mut REQUESTED_IDLE_MODE: DramIdleMode = DramIdleMode::KeepClocked;

extern "C" {
    static suspend_f: u8;
    static suspend_end: u8;
}

core::arch::global_asm!(include_str!("idle.S"));

pub fn init_idle() {
    MemoryManager::with_mut(|memory_manager| {
        memory_manager
            .map_range(
                HW_MPDDRC_BASE,
                DDRC_KERNEL_ADDR as _,
                0x1000,
                MemoryFlags::W | MemoryFlags::DEV,
                false,
            )
            .expect("unable to map DDRC")
    })
    .as_mut_ptr();

    let mut sram_virt = MemoryManager::with_mut(|memory_manager| {
        memory_manager
            .map_range(
                IDLE_FUNCTION_PHYS_ADDR,
                core::ptr::null_mut(),
                IDLE_FUNCTION_MEM_SIZE,
                MemoryFlags::W,
                false,
            )
            .expect("unable to map SRAM page for idle function")
    });
    let idle_fn_slice = unsafe {
        core::slice::from_raw_parts(&suspend_f, (&suspend_end as *const u8).offset_from(&suspend_f) as usize)
    };

    sram_virt.as_slice_mut()[..idle_fn_slice.len()].copy_from_slice(idle_fn_slice);

    // Tighten memory permissions of the idle function SRAM page WRX -> RX
    let sram_virt = MemoryManager::with_mut(|memory_manager| {
        memory_manager
            .unmap_range(sram_virt.as_ptr(), IDLE_FUNCTION_MEM_SIZE)
            .expect("unable to unmap SRAM page for idle function");
        memory_manager
            .map_range(
                IDLE_FUNCTION_PHYS_ADDR,
                core::ptr::null_mut(),
                IDLE_FUNCTION_MEM_SIZE,
                MemoryFlags::X,
                false,
            )
            .expect("unable to map SRAM page for idle function")
    });

    clean_cache_l1();
    invalidate_instruction_cache();

    unsafe {
        const THUMB_MODE_BIT: usize = 1;
        let shallow_idle_fn_address = (sram_virt.as_ptr() as usize) + THUMB_MODE_BIT;
        SUSPEND_FN =
            Some(core::mem::transmute::<usize, unsafe extern "C" fn(usize, usize)>(shallow_idle_fn_address));
    };
}

pub fn set_dram_idle_mode(_dram_idle_mode: DramIdleMode) {
    #[cfg(not(feature = "trace-systemview"))] // can't suspend DRAM if SystemView is active
    unsafe {
        REQUESTED_IDLE_MODE = _dram_idle_mode
    }
}

pub fn idle() -> bool {
    #[cfg(feature = "trace-systemview")]
    {
        systemview_keyos::SystemView::system_idle();
    }

    unsafe {
        if REQUESTED_IDLE_MODE == DramIdleMode::LowPower
            && !super::page_zeroer::RUNNING.load(core::sync::atomic::Ordering::SeqCst)
        {
            klog!("Deep idle");
            clean_cache_l1();
            clean_cache_l2();
            disable_l2_cache();
            SUSPEND_FN.unwrap()(DDRC_KERNEL_ADDR, PMC_KERNEL_ADDR);
            enable_l2_cache()
        } else {
            klog!("Shallow idle");
            armv7::asm::wfi();
        }
        klog!("Returned from idle");

        // We need to momentarily re-enable interrupts, so that they get serviced, because otherwise other
        // processes won't get marked ready, and we keep spinning in the idle loop with interrupts disabled
        // forever.
        core::arch::asm!("cpsie if", "cpsid if");
    };
    true
}
