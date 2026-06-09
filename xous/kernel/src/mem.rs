// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

#[cfg(keyos)]
use core::ptr::{addr_of, addr_of_mut};

#[cfg(keyos)]
use bitvec::array::BitArray;
pub use keyos::PAGE_SIZE;
#[cfg(keyos)]
use keyos::{
    to_plaintext_phys_addr, ENCRYPTED_DRAM_BASE, ENCRYPTED_DRAM_END, PLAINTEXT_DRAM_BASE, PLAINTEXT_DRAM_END,
    RAM_PAGES,
};
use xous::{Error, MemoryFlags, MemoryRange, PID};

pub use crate::arch::mem::MemoryMapping;
use crate::services::SystemServices;

/// Number of bytes of free memory we should have normally. Start killing processes below this amount
/// Our biggest spike in memory usage is when the bitcoin app starts with the QR scanner, using 14 + 8 MBytes
/// (plus safety margin), so we need to have at least this much free to start it without having to instantly
/// close other apps.
#[cfg(keyos)]
const LOW_MEMORY_THRESHOLD: usize = 24 * 1024 * 1024;

/// When a low memory event is reached, this many bytes need to be additionally freed before we count the low
/// memory status as resolved.
#[cfg(keyos)]
const LOW_MEMORY_HYSTERESIS: usize = 1024 * 1024;

#[derive(Debug)]
#[allow(dead_code)]
enum ClaimReleaseMove {
    Claim,
    Release,
    Move(PID /* from */),
}

#[cfg(keyos)]
fn additional_regions() -> impl Iterator<Item = (&'static str, &'static core::ops::Range<usize>)> {
    utralib::map::MEMORY_REGIONS
        .iter()
        .filter(|(name, _)| *name != "DDR_RAM" && *name != "ENCRYPTED_RAM")
        .map(|(name, range)| (*name, range))
}

#[cfg(not(keyos))]
#[derive(Default)]
pub struct MemoryManager {}

#[cfg(keyos)]
pub struct MemoryManager {
    allocations: &'static mut [Option<PID>],
    /// Bitmap of pages that have been freed but not yet zeroed
    free_pages_dirty: BitArray<[u32; RAM_PAGES / 32]>,
    /// Probable index of the next dirty page, if there is one.
    ///
    /// When allocating, we start searching at this index. When a page is freed, _and_ this is None, we set
    /// this index. This is because we free large ranges consecutively, so we will set the hint at the first
    /// page, and then leave it alone. It is also assumed that the zeroer quickly zeroes the range out, so we
    /// usually have one range of dirty pages.
    next_dirty_page_hint: Option<usize>,
    /// Bitmap of pages that are free and zeroed
    free_pages_zeroed: BitArray<[u32; RAM_PAGES / 32]>,
    /// Probable index of the next zeroed page
    ///
    /// Only updated when allocating, it is set to after the alocation.
    next_zeroed_page_hint: usize,
    /// Number of free (possibly dirty) pages
    num_free_pages: usize,
    /// True if currently in a low memory state.
    low_memory: bool,
}

#[cfg(not(keyos))]
std::thread_local!(static MEMORY_MANAGER: core::cell::RefCell<MemoryManager> = core::cell::RefCell::new(MemoryManager::default()));

#[cfg(keyos)]
static mut MEMORY_MANAGER: MemoryManager = MemoryManager::default_hack();

/// Initialize the memory map.
/// This will go through memory and map anything that the kernel is
/// using to process 1, then allocate a pagetable for this process
/// and place it at the usual offset.  The MMU will not be enabled yet,
/// as the process entry has not yet been created.
#[allow(dead_code)]
impl MemoryManager {
    #[cfg(keyos)]
    const fn default_hack() -> Self {
        MemoryManager {
            allocations: &mut [],
            free_pages_dirty: BitArray::ZERO,
            next_dirty_page_hint: None,
            free_pages_zeroed: BitArray::ZERO,
            next_zeroed_page_hint: 0,
            num_free_pages: 0,
            low_memory: false,
        }
    }

    pub fn with_mut<F, R>(f: F) -> R
    where
        F: FnOnce(&mut MemoryManager) -> R,
    {
        #[cfg(keyos)]
        unsafe {
            f(&mut *addr_of_mut!(MEMORY_MANAGER))
        }

        #[cfg(not(keyos))]
        MEMORY_MANAGER.with(|ss| f(&mut ss.borrow_mut()))
    }

    pub fn with<F, R>(f: F) -> R
    where
        F: FnOnce(&MemoryManager) -> R,
    {
        #[cfg(keyos)]
        unsafe {
            f(&*addr_of!(MEMORY_MANAGER))
        }

        #[cfg(not(keyos))]
        MEMORY_MANAGER.with(|ss| f(&ss.borrow_mut()))
    }

    #[cfg(keyos)]
    pub fn init_from_memory(&mut self, base: *mut u32) {
        use core::slice;
        let mut mem_pages = RAM_PAGES;
        for (_, range) in additional_regions() {
            mem_pages += (range.end - range.start) / PAGE_SIZE;
        }
        self.allocations = unsafe { slice::from_raw_parts_mut(base as *mut Option<PID>, mem_pages) };
        for (offset, allocation) in self.allocations[..RAM_PAGES].iter().enumerate() {
            if allocation.is_none() {
                self.free_pages_dirty.set(offset, true);
                self.num_free_pages += 1;
            }
        }
    }

    /// Print the number of RAM bytes used by the specified process.
    /// This does not include memory such as peripherals and CSRs.
    #[cfg(keyos)]
    pub fn ram_used_by(&self, pid: PID) -> usize {
        let mut owned_bytes = 0;
        #[cfg(keyos)]
        for owner in &self.allocations[0..RAM_PAGES] {
            if owner == &Some(pid) {
                owned_bytes += PAGE_SIZE;
            }
        }
        owned_bytes
    }

    /// Returns the number of free bytes in the RAM region.
    #[cfg(keyos)]
    pub fn ram_free(&self) -> usize { self.num_free_pages * PAGE_SIZE }

    #[cfg(keyos)]
    pub fn low_memory(&self) -> bool { self.low_memory }

    #[cfg(not(keyos))]
    pub fn ram_free(&self) -> usize { 64 * 1024 * 1024 }

    #[cfg(not(keyos))]
    pub fn low_memory(&self) -> bool { false }

    #[cfg(keyos)]
    pub fn print_ownership(&self, mut output: impl core::fmt::Write) {
        writeln!(output, "Ownership ({} bytes in all):", self.allocations.len()).ok();

        let mut offset = 0;
        writeln!(
            output,
            "    Region DRAM {:08x} - {:08x} {} bytes:",
            ENCRYPTED_DRAM_BASE,
            ENCRYPTED_DRAM_END,
            keyos::RAM_SIZE
        )
        .ok();

        let mut previous = None;
        for o in 0..RAM_PAGES {
            if self.allocations[offset + o] == previous {
                continue;
            }
            if let Some(_allocation) = self.allocations[offset + o] {
                crate::services::SystemServices::with(|ss| {
                    let _process_name = ss.process(_allocation).ok().and_then(|p| p.name()).unwrap_or("N/A");
                    writeln!(
                        output,
                        "        {:08x} => {} `{}`",
                        ENCRYPTED_DRAM_BASE + o * PAGE_SIZE,
                        _allocation.get(),
                        _process_name
                    )
                    .ok();
                });
            } else {
                writeln!(output, "        {:08x} => <free>", ENCRYPTED_DRAM_BASE + o * PAGE_SIZE).ok();
            }
            previous = self.allocations[offset + o];
        }

        offset += RAM_PAGES;

        // Go through additional regions looking for this address, and claim it
        // if it's not in use.
        for (name, range) in additional_regions() {
            let mut previous = None;
            let region_size = range.end - range.start;
            if region_size == 0 {
                continue;
            }
            writeln!(
                output,
                "    Region {} {:08x} - {:08x} {} bytes:",
                name, range.start, range.end, region_size
            )
            .ok();
            for o in 0..region_size / PAGE_SIZE {
                if self.allocations[offset + o] == previous {
                    continue;
                }
                if let Some(_allocation) = self.allocations[offset + o] {
                    crate::services::SystemServices::with(|ss| {
                        let _process_name =
                            ss.process(_allocation).ok().and_then(|p| p.name()).unwrap_or("N/A");
                        writeln!(
                            output,
                            "        {:08x} => {} `{}`",
                            range.start + o * PAGE_SIZE,
                            _allocation.get(),
                            _process_name,
                        )
                        .ok()
                    });
                } else {
                    writeln!(output, "        {:08x} => <free>", range.start + o * PAGE_SIZE).ok();
                }
                previous = self.allocations[offset + o];
            }
            offset += region_size / PAGE_SIZE;
        }
    }

    /// Allocate a contiguous block of encrypted physical pages to the given process.
    #[cfg(keyos)]
    #[inline(always)]
    pub fn alloc_range(&mut self, num_pages: usize, pid: PID) -> Result<(usize, bool), Error> {
        if num_pages == 1 {
            self.alloc_single_page(pid)
        } else {
            self.alloc_range_aligned::<1>(num_pages, pid)
        }
    }

    #[cfg(keyos)]
    fn alloc_single_page(&mut self, pid: PID) -> Result<(usize, bool), Error> {
        /// Area at the end of the RAM mostly reserved for POPULATE allocs. 40MB corresponds to 12 apps
        /// (including control center and keyboard), 2 framebuffer each, + 2 internal buffers for gui server,
        /// and some margin for other buffers.
        ///
        /// This is not a hard reservation, in case of high memory pressure, fragmented allocs might still
        /// spill over.
        const BIG_ALLOC_RESERVED_BYTES: usize = 40 * 1024 * 1024;

        /// Threshold for the allocation hint to go back to the start and not clobber the "Big alloc area"
        const FRAGMENTED_AREA_PAGES: usize = RAM_PAGES - BIG_ALLOC_RESERVED_BYTES / PAGE_SIZE;

        let (idx, zeroed) = if let Some(idx) =
            self.free_pages_zeroed[self.next_zeroed_page_hint..FRAGMENTED_AREA_PAGES].first_one()
        {
            self.free_pages_zeroed.set(idx + self.next_zeroed_page_hint, false);
            (idx + self.next_zeroed_page_hint, true)
        } else if let Some(idx) = self.free_pages_zeroed[..self.next_zeroed_page_hint].first_one() {
            self.free_pages_zeroed.set(idx, false);
            (idx, true)
        } else if let Some(idx) = self.free_pages_dirty[..FRAGMENTED_AREA_PAGES].first_one() {
            // Unhappy case, the zeroer cannot keep up; give out a dirty page
            self.free_pages_dirty.set(idx, false);
            (idx, false)
        } else if let Some(idx) = self.free_pages_zeroed[FRAGMENTED_AREA_PAGES..].first_one() {
            // Unhappy case #2, we start going into the big alloc area, possibly fragmenting it.
            self.free_pages_zeroed.set(FRAGMENTED_AREA_PAGES + idx, false);
            (idx + FRAGMENTED_AREA_PAGES, true)
        } else if let Some(idx) = self.free_pages_dirty[FRAGMENTED_AREA_PAGES..].first_one() {
            // Unhappy case #3, the zeroer is truly swamped, and memory is nearing full
            self.free_pages_dirty.set(FRAGMENTED_AREA_PAGES + idx, false);
            (idx + FRAGMENTED_AREA_PAGES, false)
        } else {
            return Err(Error::OutOfMemory);
        };

        let phys = ENCRYPTED_DRAM_BASE + idx * PAGE_SIZE;
        klog!("Claiming 0x{phys:08x} for PID {pid} (zeroed: {zeroed})");

        self.subtract_free_pages(1);
        self.allocations[idx] = Some(pid);
        self.next_zeroed_page_hint = idx + 1;
        if self.next_zeroed_page_hint > FRAGMENTED_AREA_PAGES {
            self.next_zeroed_page_hint = 0;
        }

        Ok((phys, zeroed))
    }

    /// Returns the physical address of the range, and if the pages are zeroed.
    #[cfg(keyos)]
    pub fn alloc_range_aligned<const ALIGN_PAGES: usize>(
        &mut self,
        num_pages: usize,
        pid: PID,
    ) -> Result<(usize, bool), Error> {
        if num_pages == 0 {
            return Err(Error::InvalidArguments);
        }
        if num_pages > RAM_PAGES {
            return Err(Error::OutOfMemory);
        }
        let end = RAM_PAGES - num_pages + 1;
        // Note that the bitmap is 4096 bytes for 128MB of RAM, and `iter_ones` scans the bitmap
        // 32 bits at a time, so it's a relatively fast linear search
        let zeroed_page_indexes = self.free_pages_zeroed[..end].iter_ones().rev();

        let (idx, zeroed) = if let Some(idx) = zeroed_page_indexes
            .filter(|i| i & (ALIGN_PAGES - 1) == 0)
            .find(|i| self.free_pages_zeroed[*i..*i + num_pages].all())
        {
            (idx, true)
        } else {
            // Slow path: mixed dirty and zeroed pages
            // Hitting this part means we are allocating and freeing so much memory that the zeroer cannot
            // keep up, so this does not need to be optimal.
            let idx = (0..end)
                .step_by(ALIGN_PAGES)
                .rev()
                .find(|i| {
                    self.free_pages_zeroed[*i..*i + num_pages]
                        .iter()
                        .zip(self.free_pages_dirty[*i..*i + num_pages].iter())
                        .all(|(zero, dirty)| *zero || *dirty)
                })
                .ok_or(Error::OutOfMemory)?;
            (idx, false)
        };

        let block_start = ENCRYPTED_DRAM_BASE + idx * PAGE_SIZE;
        klog!("Claiming 0x{block_start:08x} (0x{num_pages:x} pages) for PID {pid} (zeroed: {zeroed})");

        self.subtract_free_pages(num_pages);
        self.allocations[idx..idx + num_pages].fill(Some(pid));
        self.free_pages_zeroed[idx..idx + num_pages].fill(false);
        if !zeroed {
            self.free_pages_dirty[idx..idx + num_pages].fill(false);
        }

        Ok((block_start, zeroed))
    }

    /// Takes a dirty page region and returns its phyisical address and page count.
    ///
    /// These pages will still have None as their owner, but will not be in the free bitmap as long as they
    /// are being zeroed.
    #[cfg(keyos)]
    pub fn take_dirty_pages(&mut self) -> Option<(usize, usize)> {
        let start = self.next_dirty_page_hint.unwrap_or(0);
        let offset = if let Some(idx) = self.free_pages_dirty[start..RAM_PAGES].first_one() {
            start + idx
        } else if let Some(idx) = self.free_pages_dirty[..start].first_one() {
            idx
        } else {
            klog!("No dirty pages left");
            self.next_dirty_page_hint = None;
            return None;
        };
        let pages = self.free_pages_dirty[offset..RAM_PAGES].leading_ones().min(8 * 1024 * 1024 / PAGE_SIZE);
        self.free_pages_dirty[offset..offset + pages].fill(false);
        self.next_dirty_page_hint = Some((offset + pages) % RAM_PAGES);
        Some((ENCRYPTED_DRAM_BASE + offset * PAGE_SIZE, pages))
    }

    /// Give back pages taken with [`Self::take_dirty_pages`] after zeroing.
    #[cfg(keyos)]
    pub fn set_pages_to_zeroed(&mut self, phys: usize, pages: usize) {
        let offset = self.address_to_allocation_offset(phys).unwrap();
        self.free_pages_zeroed[offset..offset + pages].fill(true);
    }

    /// Attempt to map the given physical address into the virtual address space
    /// of this process.
    ///
    /// # Errors
    ///
    /// * MemoryInUse - The specified page is already mapped
    #[allow(unused_mut)]
    pub fn map_range(
        &mut self,
        mut phys: usize,
        virt_ptr: *mut usize,
        size: usize,
        flags: MemoryFlags,
        map_user: bool,
    ) -> Result<MemoryRange, Error> {
        let mut current_mapping = crate::arch::mem::MemoryMapping::current();
        let virt =
            SystemServices::with_mut(|ss| ss.current_process_mut().find_virtual_address(virt_ptr, size))?;
        #[cfg(keyos)]
        let mut zero_after_alloc =
            keyos::is_address_in_plaintext_dram(phys) || keyos::is_address_encrypted(phys);

        if flags.is_set(MemoryFlags::POPULATE) {
            if !flags.is_set(MemoryFlags::W) {
                return Err(Error::InvalidArguments);
            }
            if phys != 0 {
                return Err(Error::InvalidArguments);
            }
            #[cfg(keyos)]
            {
                let (allocated, zeroed) = self.alloc_range(size / PAGE_SIZE, current_mapping.get_pid())?;
                zero_after_alloc = !zeroed;
                if flags.is_set(MemoryFlags::PLAINTEXT)
                    || flags.is_set(MemoryFlags::NO_CACHE)
                    || flags.is_set(MemoryFlags::DEV)
                {
                    // Pages are "encrypted zeroed". If we read them as plaintext, they would be garbage, so
                    // we need to zero it again.
                    zero_after_alloc = true;
                    phys = to_plaintext_phys_addr(allocated)
                } else {
                    phys = allocated;
                }
            }
        } else if phys != 0 {
            // A wrapping range would leave the claim loop empty, mapping pages below
            // without an ownership check, so reject it.
            let phys_end = phys.checked_add(size).ok_or(Error::BadAddress)?;
            // 1. Attempt to claim all physical pages in the range
            for claim_phys in (phys..phys_end).step_by(PAGE_SIZE) {
                if let Err(err) = self.claim_page(claim_phys, current_mapping.get_pid()) {
                    // If we were unable to claim one or more pages, release everything and return
                    for rel_phys in (phys..claim_phys).step_by(PAGE_SIZE) {
                        self.release_page(rel_phys, current_mapping.get_pid()).ok();
                    }
                    return Err(err);
                }
            }
        }
        // Actually perform the map.  At this stage, every physical page should be owned by us.
        for offset in (0..size).step_by(PAGE_SIZE) {
            let phys_page = if phys == 0 { 0 } else { phys + offset };
            if let Err(e) = current_mapping.map_page(
                self,
                phys_page,
                virt.wrapping_add(offset / core::mem::size_of::<usize>()),
                flags,
                map_user,
            ) {
                for unmap_offset in (0..offset).step_by(PAGE_SIZE) {
                    current_mapping
                        .unmap_page(virt.wrapping_add(unmap_offset / core::mem::size_of::<usize>()))
                        .ok();
                }
                if phys != 0 {
                    for rel_phys in (phys..(phys + size)).step_by(PAGE_SIZE) {
                        self.release_page(rel_phys, current_mapping.get_pid()).ok();
                    }
                }
                return Err(e);
            }
        }

        let mut mem = unsafe { MemoryRange::new(virt as usize, size)? };

        // If we allocated DDR pages (or used POPULATE), zero it out
        #[cfg(keyos)]
        if zero_after_alloc {
            mem.as_slice_mut::<u32>().fill(0);
            current_mapping.flush_cache(mem, xous::CacheOperation::Clean)?;
        }
        Ok(mem)
    }

    /// Attempt to map the given physical address into the virtual address space
    /// of this process.
    ///
    /// # Errors
    ///
    /// * MemoryInUse - The specified page is already mapped
    pub fn map_range_readonly_mirror(
        &mut self,
        pid: PID,
        phys: usize,
        size: usize,
    ) -> Result<MemoryRange, Error> {
        SystemServices::with_mut(|ss| {
            let mut mapping = MemoryMapping::current();

            let process = &mut ss.process_mut(pid)?;
            let virt = process.next_mirror_address;

            // Actually perform the map.
            // Physical pages are owned by some other process, we're just creating a read-only mirror.
            for offset in (0..size).step_by(PAGE_SIZE) {
                if let Err(e) = mapping.map_page(
                    self,
                    offset + phys,
                    (offset + virt) as *mut usize,
                    MemoryFlags::empty(),
                    true,
                ) {
                    for unmap_offset in (0..offset).step_by(PAGE_SIZE) {
                        mapping.unmap_page((unmap_offset + virt) as *mut usize).ok();
                    }
                    return Err(e);
                }
            }

            // Update the last allocated mirror address
            let next_addr = virt.saturating_add(size);
            process.next_mirror_address = next_addr.checked_next_multiple_of(PAGE_SIZE).unwrap_or(next_addr);

            unsafe { MemoryRange::new(virt, size) }
        })
    }

    /// Unmaps pages mapped with map_range. Tries to unmap all pages and continues even if it encounters an
    /// error.
    ///
    /// # Errors
    ///
    /// * BadAddress - Address was not already mapped.
    pub fn unmap_range(&mut self, virt: *const u8, len: usize) -> Result<(), xous::Error> {
        let virt = virt as usize;
        if cfg!(keyos) && (virt & (PAGE_SIZE - 1) != 0 || len & (PAGE_SIZE - 1) != 0) {
            return Err(Error::BadAlignment);
        }
        // For large regions, one whole-cache clean+invalidate is far cheaper
        // than per-page, per-cache-line maintenance (a 2MB buffer would be
        // ~64k L2 MMIO writes; this collapses it to a single by-way pass).
        // When it fires, skip the per-page invalidate below.
        #[cfg(keyos)]
        let bulk_flushed = crate::arch::mem::MemoryMapping::current()
            .try_whole_cache_flush(len, xous::CacheOperation::CleanAndInvalidate);
        #[cfg(not(keyos))]
        let bulk_flushed = false;

        let mapping = crate::arch::mem::MemoryMapping::current();
        let mut result = Ok(());
        for addr in (virt..(virt + len)).step_by(PAGE_SIZE) {
            let addr = addr as *mut usize;
            if let Ok(phys) = mapping.virt_to_phys(addr) {
                // Invalidate the page's cache lines so the controller can't commit them later, once the
                // page has been handed to another process as non-cached. Skipped when the whole cache was
                // already flushed above.
                if !bulk_flushed {
                    mapping.invalidate_page(addr, phys);
                }
                self.release_page(phys, mapping.get_pid()).ok();
            }
            // Free the virtual address.
            if let Err(e) = mapping.unmap_page(addr) {
                if result.is_ok() {
                    result = Err(e);
                }
            }
        }
        result
    }

    /// Check if memory range is user accessible or not. Does not need to be aligned.
    ///
    /// # Errors
    ///
    /// * BadAddress - Page range was not user accessible
    pub fn check_range_accessible(&self, range: MemoryRange) -> Result<(), Error> {
        #[cfg(keyos)]
        {
            let mapping = crate::arch::mem::MemoryMapping::current();
            let start = (range.as_ptr() as usize) & !(PAGE_SIZE - 1);
            let end = (range.as_ptr() as usize)
                .checked_add(range.len())
                .and_then(|e| e.checked_next_multiple_of(PAGE_SIZE))
                .ok_or(Error::BadAddress)?;
            for addr in (start..end).step_by(PAGE_SIZE) {
                if addr > keyos::USER_AREA_END || !mapping.address_user_accessible(addr as *const usize) {
                    return Err(Error::BadAddress);
                }
            }
        }
        #[cfg(not(keyos))]
        let _ = range;
        Ok(())
    }

    /// Move a page from one process into another, keeping its permissions.
    #[allow(dead_code)]
    pub fn move_page(
        &mut self,
        src_mapping: &mut MemoryMapping,
        src_addr: *mut usize,
        dest_mapping: &mut MemoryMapping,
        dest_addr: *mut usize,
    ) -> Result<(), Error> {
        let phys_addr = src_mapping.move_page(self, src_addr, dest_mapping, dest_addr)?;
        if phys_addr != 0 {
            self.claim_release_move(
                phys_addr,
                dest_mapping.get_pid(),
                ClaimReleaseMove::Move(src_mapping.get_pid()),
            )?
        };
        Ok(())
    }

    #[allow(dead_code)]
    /// Move the page in the process mapping listing without manipulating
    /// the pagetables at all.
    pub fn move_page_raw(&mut self, phys_addr: usize, dest_pid: PID) -> Result<(), Error> {
        self.claim_release_move(
            phys_addr,
            dest_pid,
            ClaimReleaseMove::Move(crate::arch::process::current_pid()),
        )
    }

    /// Mark the page in the current process as being lent.  If the borrow is
    /// read-only, then additionally remove the "write" bit on it.  If the page
    /// is writable, then remove it from the current process until the borrow is
    /// returned.
    #[allow(dead_code)]
    pub fn lend_page(
        &mut self,
        src_mapping: &mut MemoryMapping,
        src_addr: *mut usize,
        dest_mapping: &mut MemoryMapping,
        dest_addr: *mut usize,
        mutable: bool,
    ) -> Result<(), Error> {
        // If this page is to be writable, detach it from this process.
        // Otherwise, mark it as read-only to prevent a process from modifying
        // the page while it's borrowed.
        let phys_addr = src_mapping.lend_page(self, src_addr, dest_mapping, dest_addr, mutable)?;
        // Physical ownership follows the page to the borrower, so it survives the
        // lender terminating mid-borrow, like a move.
        if phys_addr != 0 {
            self.claim_release_move(
                phys_addr,
                dest_mapping.get_pid(),
                ClaimReleaseMove::Move(src_mapping.get_pid()),
            )?
        };
        Ok(())
    }

    /// Return the range from `src_mapping` back to `dest_mapping`
    #[allow(dead_code)]
    pub fn unlend_page(
        &mut self,
        src_mapping: &mut MemoryMapping,
        src_addr: *mut usize,
        dest_mapping: &mut MemoryMapping,
        dest_addr: *mut usize,
    ) -> Result<(), Error> {
        let phys_addr = src_mapping.return_page(src_addr, dest_mapping, dest_addr)?;
        // Hand physical ownership back to the process the page is returned to.
        if phys_addr != 0 {
            self.claim_release_move(
                phys_addr,
                dest_mapping.get_pid(),
                ClaimReleaseMove::Move(src_mapping.get_pid()),
            )?
        };
        Ok(())
    }

    /// Allocate a backing page for a page that was mapped on-demand beforehand.
    #[cfg(keyos)]
    pub fn ensure_backed(&mut self, address: *mut usize) -> Result<(), Error> {
        MemoryMapping::ensure_backed(self, address)
    }

    /// Claim the given memory for the given process, or release the memory
    /// back to the free pool.
    #[cfg(not(keyos))]
    fn claim_release_move(
        &mut self,
        _addr: usize,
        _pid: PID,
        _action: ClaimReleaseMove,
    ) -> Result<(), Error> {
        Ok(())
    }

    #[cfg(keyos)]
    fn claim_release_move(&mut self, addr: usize, pid: PID, action: ClaimReleaseMove) -> Result<(), Error> {
        // Ensure the address lies on a page boundary
        if addr & 0xfff != 0 {
            return Err(Error::BadAlignment);
        }

        // RAM is ownable by any process and is never a shared peripheral, so the
        // permission check and peripheral scan are both no-ops for it. Skip them
        // -- this is the hot path (every moved/claimed RAM page), and the per-page
        // process lookup + scans otherwise dominate bulk moves.
        let is_ram = keyos::is_address_encrypted(addr) || keyos::is_address_in_plaintext_dram(addr);
        if !is_ram {
            if let Err(e) = SystemServices::with(|ss| ss.process(pid)?.check_memory_permission(addr)) {
                println!("[!] PID {pid} was denied access to hw address {addr:08x}");
                return Err(e);
            }

            // FIXME: workaround to allow to share the same peripherals
            //        between the kernel and user processes
            #[cfg(keyos)]
            {
                for base in crate::arch::mem::SHARED_PERIPHERALS.iter() {
                    if pid.get() != 1
                        && (addr == *base
                            || addr == *base + 0x1000
                            || addr == *base + 0x2000
                            || addr == *base + 0x3000)
                    {
                        klog!("[!] Peripheral sharing workaround used for {:08x} address", addr);
                        return Ok(());
                    }
                }
            }
        }

        let offset = self.address_to_allocation_offset(addr).ok_or(Error::BadAddress)?;
        match action {
            ClaimReleaseMove::Claim => {
                if self.allocations[offset].is_some() {
                    return Err(Error::MemoryInUse);
                }
                if offset < RAM_PAGES {
                    if self.free_pages_zeroed[offset] {
                        self.free_pages_zeroed.set(offset, false);
                    } else if self.free_pages_dirty[offset] {
                        self.free_pages_dirty.set(offset, false);
                    } else {
                        // No owner but not actually free: it's being zeroed right now.
                        return Err(Error::MemoryInUse);
                    }
                    self.subtract_free_pages(1);
                }
                self.allocations[offset] = Some(pid);
            }
            ClaimReleaseMove::Move(existing_pid) => {
                let Some(current_pid) = self.allocations[offset] else { return Err(Error::DoubleFree) };
                if current_pid != pid && existing_pid != current_pid {
                    return Err(Error::MemoryInUse);
                }
                self.allocations[offset] = Some(pid);
            }
            ClaimReleaseMove::Release => {
                let Some(current_pid) = self.allocations[offset] else { return Err(Error::DoubleFree) };
                if current_pid != pid {
                    return Err(Error::MemoryInUse);
                }
                if offset < RAM_PAGES {
                    self.free_pages_dirty.set(offset, true);
                    if self.next_dirty_page_hint.is_none() {
                        self.next_dirty_page_hint = Some(offset);
                    }
                    crate::platform::page_zeroer::start(self);
                    self.add_free_pages(1);
                }
                self.allocations[offset] = None;
            }
        };
        Ok(())
    }

    /// Mark a given address as being owned by the specified process ID
    fn claim_page(&mut self, addr: usize, pid: PID) -> Result<(), Error> {
        self.claim_release_move(addr, pid, ClaimReleaseMove::Claim)
    }

    /// Mark a given address as no longer being owned by the specified process ID
    fn release_page(&mut self, addr: usize, pid: PID) -> Result<(), Error> {
        self.claim_release_move(addr, pid, ClaimReleaseMove::Release)
    }

    /// Convert a physical address to an offset in the `self.allocations`, if it exists there
    #[cfg(keyos)]
    fn address_to_allocation_offset(&self, addr: usize) -> Option<usize> {
        let mut offset = 0;
        // Happy path: The address is in Encrypted RAM
        if (ENCRYPTED_DRAM_BASE..ENCRYPTED_DRAM_END).contains(&addr) {
            offset += (addr - ENCRYPTED_DRAM_BASE) / PAGE_SIZE;
            return Some(offset);
        }
        // Semi-happy path: The address is in main RAM, plaintext
        if (PLAINTEXT_DRAM_BASE..PLAINTEXT_DRAM_END).contains(&addr) {
            offset += (addr - PLAINTEXT_DRAM_BASE) / PAGE_SIZE;
            return Some(offset);
        }

        offset += RAM_PAGES;
        // Go through additional regions looking for this address
        for (_, range) in additional_regions() {
            if range.contains(&addr) {
                offset += (addr - range.start) / PAGE_SIZE;
                return Some(offset);
            }
            offset += (range.end - range.start) / PAGE_SIZE;
        }
        None
    }

    #[cfg(keyos)]
    pub fn page_owner(&self, addr: usize) -> Option<PID> {
        self.allocations[self.address_to_allocation_offset(addr)?]
    }

    /// Free all memory that belongs to a process. This does not unmap the
    /// memory from the process, it only marks it as free.
    /// This is very unsafe because the memory can immediately be re-allocated
    /// to another process, so only call this as part of destroying a process.
    pub unsafe fn release_all_memory_for_process(&mut self, _mapping: &mut MemoryMapping) {
        #[cfg(keyos)]
        for idx in 0..self.allocations.len() {
            // If this address has been allocated to this process, consider
            // freeing it or reparenting it.
            if self.allocations[idx] == Some(_mapping.get_pid()) {
                // TODO: Do not free lent pages, but instead reparent them, so that when they
                //       are returned, they can be properly freed.
                //       This can be done by walking the page tables.
                self.allocations[idx] = None;
                if idx < RAM_PAGES {
                    self.free_pages_dirty.set(idx, true);
                    self.add_free_pages(1);
                }
            }
        }
        #[cfg(keyos)]
        {
            // Clean and invalidate (not just clean) before these pages return to the
            // free pool to be DMA-zeroed and reused: a freed page must leave no cache
            // lines behind, or a later read would hit stale cached data instead of the
            // zeroer's DRAM writes. Whole-cache because a process' memory dwarfs the L2.
            crate::platform::atsama5d2::cache::clean_invalidate_cache_l1();
            crate::platform::atsama5d2::cache::clean_invalidate_cache_l2();
            // And we need to start zeroing the deallocated pages
            crate::platform::page_zeroer::start(self);
        }
    }

    #[cfg(keyos)]
    fn add_free_pages(&mut self, pages: usize) {
        assert!(self.num_free_pages.saturating_add(pages) <= RAM_PAGES);
        self.num_free_pages += pages;
        if self.low_memory
            && self.num_free_pages >= (LOW_MEMORY_THRESHOLD + LOW_MEMORY_HYSTERESIS) / PAGE_SIZE
        {
            self.low_memory = false;
        }
    }

    #[cfg(keyos)]
    fn subtract_free_pages(&mut self, pages: usize) {
        assert!(self.num_free_pages >= pages);
        self.num_free_pages -= pages;
        if !self.low_memory && self.num_free_pages < LOW_MEMORY_THRESHOLD / PAGE_SIZE {
            self.low_memory = true;
            SystemServices::with_mut(|ss| {
                ss.broadcast_event(xous::SystemEvent::LowFreeMemory, [0, 0, 0, 0]).ok()
            });
        }
    }

    /// Adjust the flags on the given memory range. This allows for stripping flags from a memory
    /// range but does not allow adding flags. The memory range must exist, and the flags must be valid.
    pub fn update_memory_flags(&mut self, _range: MemoryRange, _flags: MemoryFlags) -> Result<(), Error> {
        Err(Error::UnhandledSyscall)
    }
}
