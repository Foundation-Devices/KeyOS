use crate::{Error, MemoryRange, PAGE_SIZE};

extern crate alloc;
use alloc::alloc::{alloc_zeroed, dealloc, Layout};

/// Allocate a backing region for a memory message that just arrived from the kernel.
/// Used by the hosted arch's `receive_message` path. The kernel-side `MemoryRange` is
/// a placeholder; this gives it actual storage on the host.
pub fn alloc_message_buf(size: usize) -> core::result::Result<MemoryRange, Error> {
    let layout = Layout::from_size_align(size, PAGE_SIZE).unwrap().pad_to_align();
    let mem = unsafe { alloc_zeroed(layout) } as usize;
    unsafe { MemoryRange::new(mem, size) }
}

/// Free a backing region allocated for a memory message.
pub fn free_message_buf(range: MemoryRange) -> core::result::Result<(), Error> {
    let layout = Layout::from_size_align(range.len(), PAGE_SIZE).unwrap().pad_to_align();
    let ptr = range.as_mut_ptr();
    unsafe { dealloc(ptr, layout) };
    Ok(())
}
