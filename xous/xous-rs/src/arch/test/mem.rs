use crate::{Error, MemoryAddress, MemoryRange};

extern crate alloc;
use alloc::alloc::{alloc, dealloc, Layout};

/// Allocate a backing region for a memory message that just arrived from the kernel.
/// Used by the test arch's `receive_message` path. The kernel-side `MemoryRange` is a
/// placeholder; this gives it actual storage on the host.
pub fn alloc_message_buf(size: usize) -> core::result::Result<MemoryRange, Error> {
    let layout = Layout::from_size_align(size, crate::PAGE_SIZE).unwrap();
    let new_mem = MemoryAddress::new(unsafe { alloc(layout) } as usize).ok_or(Error::BadAddress)?;
    unsafe { MemoryRange::new(new_mem.get(), size) }
}

/// Free a backing region allocated for a memory message.
pub fn free_message_buf(range: MemoryRange) -> core::result::Result<(), Error> {
    let layout = Layout::from_size_align(range.len(), crate::PAGE_SIZE).unwrap();
    let ptr = range.as_mut_ptr();
    unsafe { dealloc(ptr, layout) };
    Ok(())
}
