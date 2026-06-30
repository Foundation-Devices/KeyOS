core::arch::global_asm!(
    include_str!("asm.S"),

    KERNEL_MODE_STACK_TOP = const keyos::KERNEL_MODE_STACK_BOTTOM,
);

pub fn flush_tlb_entry(mva: *mut usize) {
    unsafe {
        core::arch::asm!(
            // Ensure the descriptor write is visible to the table walk before
            // we invalidate (page tables are now cacheable).
            "dsb",
            // Invalidate unified TLB entries by MVA all ASID
            "mcr p15, 0, {mva}, c8, c7, 3",
            // Make sure the invalidate completed, then resync the pipeline so
            // subsequent fetches use the new translation.
            "dsb",
            "isb",
            mva = in(reg) mva
        );
    }
}
