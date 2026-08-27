use armv7::asm::{dsb, isb};

core::arch::global_asm!(
    include_str!("asm.S"),

    KERNEL_MODE_STACK_TOP = const keyos::KERNEL_MODE_STACK_BOTTOM,
);

/// Invalidate the TLB entry for `mva`, in every ASID.
pub fn flush_tlb_entry(mva: *const usize) {
    // Page tables are cacheable, so the descriptor write has to be visible to the
    // table walk before we invalidate.
    dsb();
    // SAFETY: TLBIMVAA only invalidates TLB entries, it touches no memory or flags.
    unsafe {
        core::arch::asm!(
            // Invalidate unified TLB entries by MVA all ASID
            "mcr p15, 0, {mva}, c8, c7, 3",
            mva = in(reg) mva,
            options(nomem, nostack, preserves_flags),
        );
    }
    // Make sure the invalidate completed, then resync the pipeline so subsequent
    // fetches use the new translation.
    dsb();
    isb();
}
