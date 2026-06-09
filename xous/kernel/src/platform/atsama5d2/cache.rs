// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use atsama5d27::{
    l1cache,
    l2cc::{Counter, EventCounterKind, L2cc},
    sfr::Sfr,
};
use keyos::{L2CC_KERNEL_ADDR, SFR_KERNEL_ADDR};
use utralib::HW_SFR_BASE;
use xous::{CacheOperation, MemoryFlags};

use crate::mem::MemoryManager;

static mut L2CC_KERNEL: Option<L2cc> = None;

pub(crate) fn init_l1() {
    l1cache::enable_icache();
    l1cache::enable_dcache();

    assert!(l1cache::is_icache_enabled(), "L1 I-cache should be enabled");
    assert!(l1cache::is_dcache_enabled(), "L1 D-cache should be enabled");
}

pub(crate) fn init_l2() {
    map_sfr();
    let mut sfr = Sfr::with_alt_base_addr(SFR_KERNEL_ADDR as u32);
    sfr.set_l2_cache_sram_enabled(true);
    assert!(sfr.l2_cache_sram_enabled(), "L2 cache sram should be enabled");
    unmap_sfr();

    // This assumes L2CC is mapped into the memory space by the loader
    let mut l2cc = L2cc::with_alt_base_addr(L2CC_KERNEL_ADDR as u32);
    assert!(!l2cc.is_enabled(), "L2 cache should be disabled");
    l2cc.set_data_prefetch_enable(true);
    l2cc.set_inst_prefetch_enable(true);
    l2cc.set_double_line_fill_enable(true);
    l2cc.set_force_write_alloc(0);
    l2cc.set_prefetch_offset(1);
    l2cc.set_prefetch_drop_enable(true);
    l2cc.set_standby_mode_enable(true);
    l2cc.set_dyn_clock_gating_enable(true);
    l2cc.enable_event_counter(Counter::Counter0, EventCounterKind::DrHit);
    l2cc.enable_event_counter(Counter::Counter1, EventCounterKind::DwHit);
    l2cc.invalidate_way(0xFF);
    l2cc.cache_sync();
    l2cc.set_exclusive(false);
    l2cc.set_enable(true);

    assert!(l2cc.is_enabled(), "L2 cache controller isn't enabled");

    unsafe {
        L2CC_KERNEL = Some(l2cc);
    }
}

fn map_sfr() {
    MemoryManager::with_mut(|memory_manager| {
        memory_manager
            .map_range(
                HW_SFR_BASE,
                SFR_KERNEL_ADDR as *mut usize,
                0x1000,
                MemoryFlags::W | MemoryFlags::DEV,
                false,
            )
            .expect("unable to map SFR to kernel")
    });
}

fn unmap_sfr() {
    MemoryManager::with_mut(|memory_manager| {
        memory_manager.unmap_range(SFR_KERNEL_ADDR as _, 0x1000).expect("unable to unmap SFR in the kernel");
    });
}

#[cfg(any(not(feature = "production"), feature = "log-serial"))]
pub fn print_l2cache_stats() {
    unsafe {
        if let Some(l2cc) = (&mut *core::ptr::addr_of_mut!(L2CC_KERNEL)).as_mut() {
            println!("\tLevel 2 cache stats since the last request:");
            println!("\tD-cache read hits: {}", l2cc.get_event_count(Counter::Counter0));
            println!("\tD-cache write hits: {}", l2cc.get_event_count(Counter::Counter1));

            l2cc.reset_event_count(Counter::Counter0);
            l2cc.reset_event_count(Counter::Counter1);
        }
    }
}

/// Cleans and/or invalidates the L1 data cache.
pub fn clean_cache_l1() { l1cache::clean_dcache() }

/// Cleans and invalidates the entire L1 data cache.
pub fn clean_invalidate_cache_l1() { l1cache::clean_invalidate_dcache() }

/// Cleans and/or invalidates the L1 instruction cache.
pub fn invalidate_instruction_cache() { l1cache::invalidate_icache() }

/// Cleans and/or invalidates a memory region from L1 caches.
/// Neither end have to be aligned.
pub fn flush_cache_region_l1(virt_start: u32, virt_end: u32, op: CacheOperation) {
    match op {
        CacheOperation::Clean => l1cache::clean_region_dcache(virt_start, virt_end),
        CacheOperation::Invalidate => l1cache::invalidate_region_dcache(virt_start, virt_end),
        CacheOperation::CleanAndInvalidate => l1cache::clean_invalidate_region_dcache(virt_start, virt_end),
    };
}

/// Cleans and/or invalidates the L2 cache
pub fn clean_cache_l2() {
    if let Some(l2cc) = unsafe { (&mut *core::ptr::addr_of_mut!(L2CC_KERNEL)).as_mut() } {
        l2cc.cache_clean()
    }
}

/// Cleans and invalidates the entire L2 cache.
pub fn clean_invalidate_cache_l2() {
    if let Some(l2cc) = unsafe { (&mut *core::ptr::addr_of_mut!(L2CC_KERNEL)).as_mut() } {
        l2cc.cache_clean_invalidate()
    }
}

/// Cleans and/or invalidates a memory region from L2 caches
/// Neither end have to be aligned.
pub fn flush_cache_region_l2(phys_start: u32, phys_end: u32, op: CacheOperation) {
    if let Some(l2cc) = unsafe { (&mut *core::ptr::addr_of_mut!(L2CC_KERNEL)).as_mut() } {
        match op {
            CacheOperation::Clean => l2cc.clean_region(phys_start, phys_end),
            CacheOperation::Invalidate => l2cc.invalidate_region(phys_start, phys_end),
            CacheOperation::CleanAndInvalidate => l2cc.clean_invalidate_region(phys_start, phys_end),
        }
    }
}

pub fn disable_l2_cache() {
    if let Some(l2cc) = unsafe { (&mut *core::ptr::addr_of_mut!(L2CC_KERNEL)).as_mut() } {
        l2cc.set_enable(false);
    }
}

pub fn enable_l2_cache() {
    if let Some(l2cc) = unsafe { (&mut *core::ptr::addr_of_mut!(L2CC_KERNEL)).as_mut() } {
        l2cc.set_enable(true);
    }
}
