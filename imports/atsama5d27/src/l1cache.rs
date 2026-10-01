//! Level 1 cache.

use core::arch::asm;

const L1_CACHE_WAYS: u32 = 4;
const L1_CACHE_SETS: u32 = 256;
pub const L1_CACHE_BYTES: u32 = 32;

/// SCTLR: I - I-cache enable/disable
/// * 0 = I-cache disabled
/// * 1 = I-cache enabled
const CP15_SCTLR_I: u32 = 1 << 12;

/// SCTLR: C - D-cache enable/disable
/// * 0 = D-cache disabled
/// * 1 = D-cache enabled
const CP15_SCTLR_C: u32 = 1 << 2;

/// SCTLR: M - MMU enable/disable
/// * 0 = disabled
/// * 1 = enabled
const CP15_SCTLR_M: u32 = 1 << 0;

#[inline]
pub fn enable_icache() {
    let sctlr = read_cp15_sctlr();
    if sctlr & CP15_SCTLR_I == 0 {
        invalidate_icache();
        write_cp15_sctlr(sctlr | CP15_SCTLR_I);
    }
}

#[inline]
pub fn disable_icache() {
    let sctlr = read_cp15_sctlr();
    if (sctlr & CP15_SCTLR_I) != 0 {
        write_cp15_sctlr(sctlr & !CP15_SCTLR_I);
        invalidate_icache();
    }
}

#[inline]
pub fn is_icache_enabled() -> bool {
    let sctlr = read_cp15_sctlr();
    sctlr & CP15_SCTLR_I != 0
}

#[inline]
pub fn enable_dcache() {
    let sctlr = read_cp15_sctlr();
    if (sctlr & CP15_SCTLR_C) == 0 {
        assert_ne!(sctlr & CP15_SCTLR_M, 0, "MMU must be enabled");
        invalidate_dcache();
        write_cp15_sctlr(sctlr | CP15_SCTLR_C);
    }
}

#[inline]
pub fn disable_dcache() {
    let sctlr = read_cp15_sctlr();
    if (sctlr & CP15_SCTLR_C) != 0 {
        clean_dcache();
        write_cp15_sctlr(sctlr & !CP15_SCTLR_C);
        invalidate_dcache();
    }
}

#[inline]
pub fn is_dcache_enabled() -> bool {
    let sctlr = read_cp15_sctlr();
    sctlr & (CP15_SCTLR_C | CP15_SCTLR_M) == (CP15_SCTLR_C | CP15_SCTLR_M)
}

#[inline]
pub fn invalidate_icache() {
    let val = 0;
    unsafe {
        asm!(
            "mcr p15, 0, {}, c7, c5, 0",
            "dsb",
            "isb",
            in(reg) val,
        )
    }
}

#[inline]
pub fn invalidate_dcache() {
    for way in 0..L1_CACHE_WAYS {
        for set in 0..L1_CACHE_SETS {
            cp15_dcache_invalidate_setway(set, way);
        }
    }

    armv7::asm::dsb();
}

#[inline]
pub fn clean_dcache() {
    for way in 0..L1_CACHE_WAYS {
        for set in 0..L1_CACHE_SETS {
            cp15_dcache_clean_setway(set, way);
        }
    }

    armv7::asm::dsb();
}

#[inline]
pub fn clean_invalidate_dcache() {
    for way in 0..L1_CACHE_WAYS {
        for set in 0..L1_CACHE_SETS {
            cp15_dcache_clean_invalidate_setway(set, way);
        }
    }

    armv7::asm::dsb();
}

#[inline]
pub fn invalidate_region_dcache(start: u32, end: u32) {
    let start = start & !(L1_CACHE_BYTES - 1);
    for mva in (start..end).step_by(L1_CACHE_BYTES as usize) {
        cp15_dcache_invalidate_mva(mva);
    }

    armv7::asm::dsb();
}

#[inline]
pub fn clean_region_dcache(start: u32, end: u32) {
    let start = start & !(L1_CACHE_BYTES - 1);
    for mva in (start..end).step_by(L1_CACHE_BYTES as usize) {
        cp15_dcache_clean_mva(mva);
    }

    armv7::asm::dsb();
}

#[inline]
pub fn clean_invalidate_region_dcache(start: u32, end: u32) {
    let start = start & !(L1_CACHE_BYTES - 1);
    for mva in (start..end).step_by(L1_CACHE_BYTES as usize) {
        cp15_dcache_clean_invalidate_mva(mva);
    }

    armv7::asm::dsb();
}

fn read_cp15_sctlr() -> u32 {
    let mut res;
    unsafe {
        asm!(
            "mrc p15, 0, {}, c1, c0, 0",
            out(reg) res,
        )
    }
    res
}

fn write_cp15_sctlr(value: u32) {
    unsafe {
        asm!(
            "mcr p15, 0, {}, c1, c0, 0",
            in(reg) value,
        )
    }
}

fn cp15_dcache_invalidate_mva(mva: u32) {
    unsafe {
        asm!(
            "mcr p15, 0, {}, c7, c6, 1",
            in(reg) mva,
        )
    }
}

fn cp15_dcache_clean_mva(mva: u32) {
    unsafe {
        asm!(
            "mcr p15, 0, {}, c7, c10, 1",
            in(reg) mva,
        )
    }
}

fn cp15_dcache_clean_invalidate_mva(mva: u32) {
    unsafe {
        asm!(
            "mcr p15, 0, {}, c7, c14, 1",
            in(reg) mva,
        )
    }
}

fn cp15_dcache_invalidate_setway(set: u32, way: u32) {
    let setway = (set << 5) | (way << 30);
    unsafe {
        asm!(
        "mcr p15, 0, {}, c7, c6, 2",
        in(reg) setway,
        )
    }
}

fn cp15_dcache_clean_setway(set: u32, way: u32) {
    let setway = (set << 5) | (way << 30);
    unsafe {
        asm!(
            "mcr p15, 0, {}, c7, c10, 2",
            in(reg) setway,
        )
    }
}

fn cp15_dcache_clean_invalidate_setway(set: u32, way: u32) {
    let setway = (set << 5) | (way << 30);
    unsafe {
        asm!(
            "mcr p15, 0, {}, c7, c14, 2",
            in(reg) setway,
        )
    }
}
