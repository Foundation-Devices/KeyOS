static EXCEPTION_STACK_TOP: u32 = keyos::EXCEPTION_STACK_BOTTOM as u32;
static IRQ_STACK_TOP: u32 = keyos::IRQ_STACK_BOTTOM as u32;
static THREAD_CONTEXT_AREA: u32 = keyos::THREAD_CONTEXT_AREA as u32;

core::arch::global_asm!(
    include_str!("asm.S"),

    EXCEPTION_STACK_TOP = sym EXCEPTION_STACK_TOP,
    IRQ_STACK_TOP = sym IRQ_STACK_TOP,
    THREAD_CONTEXT_AREA = sym THREAD_CONTEXT_AREA,
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
