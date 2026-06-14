// SPDX-FileCopyrightText: 2022 Foundation Devices <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use core::num::NonZeroU8;

use armv7::{
    structures::paging::{
        InMemoryRegister, PageTableDescriptor, PageTableType, Readable, TranslationTableDescriptor,
        TranslationTableType, PAGE_TABLE_FLAGS, SMALL_PAGE_FLAGS,
    },
    PhysicalAddress, VirtualAddress,
};
use keyos::{
    is_address_in_plaintext_dram, to_plaintext_phys_addr, ENCRYPTED_DRAM_BASE, ENCRYPTED_DRAM_END,
    L1_USER_PAGE_TABLE_ENTRIES, L1_USER_PAGE_TABLE_PAGES, MAPPED_PHYSICAL_RAM, PAGE_SIZE, TTBR1_SPLIT,
};
use xous::{CacheOperation, Error, MemoryFlags, MemoryRange, PID};
#[cfg(any(not(feature = "production"), feature = "log-serial"))]
use {
    armv7::structures::paging::{PageTable, TranslationTable, PAGE_TABLE_SIZE},
    keyos::{
        ALLOCATION_TRACKER_OFFSET, ALLOCATION_TRACKER_PAGES_MAX, EXCEPTION_STACK_BOTTOM,
        EXCEPTION_STACK_PAGE_COUNT, IRQ_STACK_BOTTOM, IRQ_STACK_PAGE_COUNT, KERNEL_LOAD_OFFSET,
        KERNEL_STACK_BOTTOM, KERNEL_STACK_PAGE_COUNT, MEMORY_MIRROR_AREA_VIRT, NUM_KERNEL_PAGES_MAX,
    },
};

use crate::{
    arch::arm::asm::flush_tlb_entry,
    mem::{ClearShared, MemoryManager},
};

pub const DEFAULT_MEMORY_MAPPING: MemoryMapping =
    MemoryMapping { ttbr0: PhysicalAddress::new(0), pid: PID::new(1).unwrap() };

pub const SHARED_PERIPHERALS: [usize; 7] = [
    crate::platform::atsama5d2::uart::UartType::BASE_ADDRESS,
    utralib::HW_SFC_BASE, // Used by both `settings` and the kernel (to determine board revision)
    utralib::HW_TRNG_BASE,
    utralib::HW_PIO_BASE,
    utralib::HW_RSTC_BASE,   // Share SCKC peripheral between gpio irqs and kernel
    utralib::HW_PMC_BASE,    // Used for idle function in the kernel
    utralib::HW_SECURAM_MEM, // Used by `security`, `crypto`, and the kernel
];

// L2 cache is 128KB. If we are flushing something bigger than this, it's easier to just flush the whole cache
// instead of going page by page, line by line.
const FULL_CACHE_FLUSH_THRESHOLD: usize = 128 * 1024;

// TTBR translation-table-walk attributes: inner+outer Write-Back Write-Allocate,
// Non-shareable (IRGN bit6, RGN bit3, S=0). Must match the page-table window's
// memory type. Non-shareable because ACTLR.SMP=0 (shareable WB-WA would be
// treated as Non-cacheable on this A5). Keep in sync with loader/src/asm.S.
const TTBR_WALK_ATTRS: u32 = 0x48;

#[derive(Debug)]
pub struct MemoryMapping {
    ttbr0: PhysicalAddress,
    pid: PID,
}

impl Default for MemoryMapping {
    fn default() -> Self { DEFAULT_MEMORY_MAPPING }
}

/// The actual workhorse behind [`MemoryMapping`], the L2 table entries.
/// All variants map down to u32 that can then be written to the hardware
/// page table.
#[derive(Debug, Clone, Copy)]
enum L2TableEntry {
    /// Free slot, not allocated or mapped.
    Empty,
    /// Mapped page with a real physical page behind it. Can be used by the MMU.
    Mapped(PageTableDescriptor),
    /// Unmapped but allocated page, used for on-demand mapped or shared pages.
    /// When written into the page table, it translates into an invalid entry
    /// (bottom two bits are 0), so that will be unused by the MMU and give a
    /// translation fault.
    /// Stores MemoryFlags and other metadata as-is, in a custom bitfield format.
    /// phys()==0 means on-demand page, phys()!=0 means shared, but use
    /// [`L2TableEntry::is_shared`] instead.
    Unmapped(usize),
}

impl MemoryMapping {
    pub unsafe fn allocate(&mut self, pid: PID) -> Result<(), Error> {
        if self.ttbr0.as_u32() != 0 {
            return Err(Error::MemoryInUse);
        }

        // Allocate a new L1 page table
        let (l1_pt_phys, zeroed) = MemoryManager::with_mut(|mm| {
            // ARMv7A Level 1 Translation Table is required to be physically aligned at 16K boundary
            mm.alloc_range_aligned::<4>(L1_USER_PAGE_TABLE_PAGES, pid)
        })?;
        let l1_pt_phys = PhysicalAddress::new(l1_pt_phys as u32);
        klog!(
            "Allocated {} new pages for a new L1 table: phys={:08x?}",
            L1_USER_PAGE_TABLE_PAGES,
            l1_pt_phys
        );

        if !zeroed {
            Self::zero_newly_allocated_tt_pages(l1_pt_phys, L1_USER_PAGE_TABLE_PAGES);
        }

        self.pid = pid;
        self.ttbr0 = l1_pt_phys;

        Ok(())
    }

    /// Get the currently active memory mapping.
    pub fn current() -> MemoryMapping {
        let mut ttbr0: u32;
        let mut pid: usize;

        unsafe {
            core::arch::asm!(
                "mrc p15, 0, {ttbr0}, c2, c0, 0",
                "mrc p15, 0, {pid}, c13, c0, 1",
                ttbr0 = out(reg) ttbr0,
                pid = out(reg) pid,
            )
        }

        assert_ne!(pid, 0, "Hardware PID is zero");

        MemoryMapping {
            // Strip the TTBR cacheability/shareability attribute bits; the table
            // base is 16KiB-aligned so the low 14 bits are ours to mask.
            ttbr0: PhysicalAddress::new(ttbr0 & !0x3fff),
            pid: unsafe { NonZeroU8::new_unchecked((pid & 0xff) as u8) },
        }
    }

    pub fn get_pid(&self) -> PID { self.pid }

    pub fn is_kernel(&self) -> bool { self.pid.get() == 1 }

    pub fn activate(&self) {
        klog!("Activating current memory mapping. ttbr0: {:08x}, pid: {}", self.ttbr0, self.pid.get(),);
        let contextidr = ((self.pid.get() as usize) << 8) | self.pid.get() as usize;
        let zero = 0;
        unsafe {
            // Set TTBR0 and CONTEXTIDR
            // Performs the synchronization described in ARM B3.10.4,
            // and cache maintenance requirements in ARM B3.11.2
            core::arch::asm!(
              "mcr p15, 0, {zero}, c13, c0, 1",
              "isb",
              "mcr p15, 0, {ttbr0}, c2, c0, 0",
              "isb",
              "mcr p15, 0, {contextidr}, c13, c0, 1",
              "isb",
              zero = in(reg) zero,
              ttbr0 = in(reg) self.ttbr0.as_u32() | TTBR_WALK_ATTRS,
              contextidr = in(reg) contextidr,
            );
        }
    }

    pub fn destroy(&mut self) {
        let asid = self.pid.get() as usize;
        unsafe {
            core::arch::asm!(
                // Flush TLB by ASID, so we don't use stale entries later
                "mcr p15, 0, {asid}, c8, c7, 2",
                "isb",
                asid = in(reg) asid,
            )
        }

        self.ttbr0 = PhysicalAddress::new(0);
    }

    /// Pointer to the L1 translation-table descriptor for section index `vpn1`.
    fn l1_entry_ptr(&self, vpn1: usize) -> *mut TranslationTableDescriptor {
        let l1_pt_addr = transparent_phys_to_virt(self.ttbr0);
        unsafe { l1_pt_addr.add(vpn1) as *mut TranslationTableDescriptor }
    }

    /// Get the L2 pagetable entry for a given address, or `Err()` if the address is not mapped in L1
    /// The entry itself may be empty.
    fn get_l2_entry(&self, addr: *const usize) -> Result<*mut u32, Error> {
        if addr as usize & (PAGE_SIZE - 1) != 0 {
            return Err(Error::BadAlignment);
        }

        let v = VirtualAddress::new(addr as u32);
        let vpn1 = v.translation_table_index();
        let vpn2 = v.page_table_index();
        assert!(vpn1 < 4096);
        assert!(vpn2 < 256);

        if !self.is_kernel() && vpn1 >= L1_USER_PAGE_TABLE_ENTRIES {
            return Err(Error::AccessDenied);
        }

        let existing_l1_entry = unsafe { self.l1_entry_ptr(vpn1).read_volatile() };
        if existing_l1_entry.get_type() == TranslationTableType::Invalid {
            return Err(Error::BadAddress);
        }
        let l2_pt_addr = transparent_phys_to_virt(existing_l1_entry.get_addr().unwrap());
        Ok(unsafe { l2_pt_addr.add(vpn2) })
    }

    fn zero_newly_allocated_tt_pages(pt_phys: PhysicalAddress, pages: usize) {
        // Zero the pages out through the transparent mapping
        let slice = unsafe {
            core::slice::from_raw_parts_mut::<u32>(
                transparent_phys_to_virt(pt_phys),
                pages * PAGE_SIZE / core::mem::size_of::<u32>(),
            )
        };
        slice.fill(0);
        // The transparent mapping is cacheable, so the zero-fill sits in the
        // cache; order it before anything points the walker at this table.
        unsafe { core::arch::asm!("dsb") };
    }

    /// Create an L2 table entry (and L2 table if necessary).
    /// Returns an Error if the address is in use, even if it is unmapped currently.
    fn allocate_l2_entry(&self, mm: &mut MemoryManager, virt: *mut usize) -> Result<*mut u32, Error> {
        if virt as usize & (PAGE_SIZE - 1) != 0 {
            return Err(Error::BadAlignment);
        }
        let v = VirtualAddress::new(virt as u32);
        let vpn1 = v.translation_table_index();
        let vpn2 = v.page_table_index();

        assert!(vpn1 < 4096);
        assert!(vpn2 < 256);

        if !self.is_kernel() && vpn1 >= L1_USER_PAGE_TABLE_ENTRIES {
            return Err(Error::AccessDenied);
        }

        let existing_l1_entry = unsafe { self.l1_entry_ptr(vpn1).read_volatile() };

        let l2_pt_phys = match existing_l1_entry.get_type() {
            TranslationTableType::Invalid => {
                // Pack four 1KiB L2 tables into one 4KiB page by backing the whole
                // aligned 4-section group, rather than wasting 3KiB per section. The
                // loader and this function only ever create L2 tables a full group at
                // a time, so an Invalid entry guarantees its siblings are Invalid too;
                // no need to check them.
                const L2_TABLE_BYTES: u32 = 256 * 4;
                let (page_phys, zeroed) = mm.alloc_range(1, self.pid)?;
                klog!("Allocated a new page for four L2 tables: phys={:08x?}", page_phys);
                let page_phys = page_phys as u32;

                if !zeroed {
                    Self::zero_newly_allocated_tt_pages(PhysicalAddress::new(page_phys), 1);
                }

                let attributes =
                    u32::from(PAGE_TABLE_FLAGS::VALID::Enable) | u32::from(PAGE_TABLE_FLAGS::DOMAIN.val(0xf));
                let group_base = vpn1 & !0b11;
                for k in 0..4u32 {
                    let sub_table = PhysicalAddress::new(page_phys + k * L2_TABLE_BYTES);
                    let descriptor =
                        TranslationTableDescriptor::new(TranslationTableType::Page, sub_table, attributes)
                            .expect("tt descriptor");
                    unsafe { self.l1_entry_ptr(group_base + k as usize).write_volatile(descriptor) };
                }
                // Make the new L1 descriptors visible to the table walker.
                unsafe { core::arch::asm!("dsb") };

                PhysicalAddress::new(page_phys + (vpn1 & 0b11) as u32 * L2_TABLE_BYTES)
            }
            TranslationTableType::Page => existing_l1_entry.get_addr().unwrap(),
            _ => return Err(Error::MemoryInUse),
        };
        let l2_entry = transparent_phys_to_virt(l2_pt_phys).wrapping_add(vpn2);

        let existing_l2_entry = L2TableEntry::read_from(l2_entry);
        if !existing_l2_entry.is_empty() {
            klog!(
                "Page {:08x} already mapped: {:08x?} (table entry @{:08x})!",
                virt as usize,
                existing_l2_entry,
                l2_entry as usize
            );
            return Err(Error::MemoryInUse);
        }
        Ok(l2_entry)
    }

    /// Map the given page to the specified process table.
    /// Does not allocate actual physical pages to back the mapping.
    /// Use existing physical addresses, or phys=0 and [`MemoryMapping::ensure_backed`]
    /// to make sure there is a page behind the entry.
    pub fn map_page(
        &mut self,
        mm: &mut MemoryManager,
        phys: usize,
        virt: *mut usize,
        flags: MemoryFlags,
        map_user: bool,
    ) -> Result<(), Error> {
        if flags.is_set(MemoryFlags::W | MemoryFlags::X) {
            panic!("Tried to map RWX page! phys=0x{phys:08x}, virt=0x{:08x}, user={map_user}", virt as usize);
        }

        klog!(
            "map_page(): pid={} phys={:08x} virt={:08x}, flags: {:04x}",
            self.pid.get(),
            phys,
            virt as usize,
            flags.bits()
        );
        let entry_data = if phys != 0 {
            L2TableEntry::new_mapped(virt, phys, flags, map_user)?
        } else {
            L2TableEntry::new_on_demand(phys, flags, map_user)?
        };

        entry_data.write_to(virt, self.allocate_l2_entry(mm, virt)?);

        Ok(())
    }

    /// Ummap the given page from the specified process table.
    /// Returns an error if the page was not allocated or is currently shared with another process.
    /// Doesn't return an error for unmapped (allocated but not yet backed) pages.
    pub fn unmap_page(&self, virt: *mut usize) -> Result<(), Error> {
        let entry = self.get_l2_entry(virt)?;
        let entry_data = L2TableEntry::read_from(entry);
        if entry_data.is_empty() || entry_data.is_shared() {
            return Err(Error::BadAddress);
        };
        L2TableEntry::Empty.write_to(virt, entry);

        Ok(())
    }

    /// Move a page from one address space to another.
    /// The page cannot be shared (unmapped but has phys address)
    pub fn move_page(
        &mut self,
        mm: &mut MemoryManager,
        src_addr: *mut usize,
        dest_space: &mut MemoryMapping,
        dest_addr: *mut usize,
    ) -> Result<usize, Error> {
        klog!("***move - src: {:08x} dest: {:08x}***", src_addr as u32, dest_addr as u32);
        let src_entry = self.get_l2_entry(src_addr)?;
        let mut entry_data = L2TableEntry::read_from(src_entry);
        if !self.is_kernel() && !entry_data.is_user_accessible() {
            return Err(Error::AccessDenied);
        }
        // An on-demand source must be backed before it can be moved; back it
        // from the walk we already did.
        if entry_data.is_on_demand() {
            self.allocate_backing_page(mm, src_addr, src_entry, entry_data)?;
            entry_data = L2TableEntry::read_from(src_entry);
        }
        if !entry_data.is_mapped() && entry_data.phys()? != 0 {
            return Err(Error::BadAddress);
        }
        let dest_entry = dest_space.allocate_l2_entry(mm, dest_addr)?;

        L2TableEntry::Empty.write_to(src_addr, src_entry);

        entry_data.write_to(dest_addr, dest_entry);

        Ok(entry_data.phys()?)
    }

    /// Lend a page from one address space to another.
    /// The source page must be backed by a physical page.
    /// The source page will become unmapped (and marked as shared) until it is returned.
    pub fn lend_page(
        &mut self,
        mm: &mut MemoryManager,
        src_addr: *mut usize,
        dest_space: &mut MemoryMapping,
        dest_addr: *mut usize,
        mutable: bool,
    ) -> Result<usize, Error> {
        klog!("***lend - src: {:08x} dest: {:08x}***", src_addr as u32, dest_addr as u32);
        let src_entry = self.get_l2_entry(src_addr)?;
        let mut entry_data = L2TableEntry::read_from(src_entry);
        if !self.is_kernel() && !entry_data.is_user_accessible() {
            return Err(Error::AccessDenied);
        }
        // Lending requires a backed source, so back the on-demand page from
        // the walk we already did.
        if entry_data.is_on_demand() {
            self.allocate_backing_page(mm, src_addr, src_entry, entry_data)?;
            entry_data = L2TableEntry::read_from(src_entry);
        }
        // Note: we could probably get away with "moving" unmapped pages here,
        //       but some code might depend on moved and lent pages being actually
        //       backed.
        if !entry_data.is_mapped() {
            return Err(Error::BadAddress);
        }
        let dest_entry = dest_space.allocate_l2_entry(mm, dest_addr)?;

        entry_data.to_unmapped()?.write_to(src_addr, src_entry);

        let new_data = if !mutable { entry_data.to_immutable()? } else { entry_data };
        new_data.write_to(dest_addr, dest_entry);

        entry_data.phys()
    }

    /// Return a page from `src_space` back to `dest_space`.
    /// The source page must be backed by a physical page.
    /// Pages are checked to be the same as they were in [`MemoryMapping::lend_page`], and
    /// an error is returned if there's a mixup.
    pub fn return_page(
        &mut self,
        src_addr: *mut usize,
        dest_space: &mut MemoryMapping,
        dest_addr: *mut usize,
    ) -> Result<usize, Error> {
        klog!("***return - src: {:08x} dest: {:08x}***", src_addr as u32, dest_addr as u32);
        let src_entry = self.get_l2_entry(src_addr)?;
        let dest_entry = dest_space.get_l2_entry(dest_addr)?;

        let src_data = L2TableEntry::read_from(src_entry);
        let dest_data = L2TableEntry::read_from(dest_entry);
        if src_data.phys()? != dest_data.phys()? {
            klog!("Trying to return wrong page: src: {src_data:?}, dest: {dest_data:?}");
            return Err(Error::ShareViolation);
        }

        L2TableEntry::Empty.write_to(src_addr, src_entry);

        dest_data.to_mapped(dest_addr)?.write_to(dest_addr, dest_entry);
        dest_data.phys()
    }

    /// Drop a lent (shared) reservation, freeing the slot or leaving an unbacked on-demand
    /// entry the lender can still unmap. The physical page belongs to the borrower now, so
    /// it is never freed here.
    pub fn clear_shared(&mut self, addr: *mut usize, mode: ClearShared) -> Result<(), Error> {
        let entry = self.get_l2_entry(addr)?;
        let data = L2TableEntry::read_from(entry);
        if !data.is_shared() {
            return Ok(());
        }
        match mode {
            ClearShared::Free => L2TableEntry::Empty.write_to(addr, entry),
            ClearShared::OnDemand => data.with_phys(0)?.write_to(addr, entry),
        }
        Ok(())
    }

    /// Get the physical address of a virtual one.
    /// Returns various errors if the address is not mapped.
    pub fn virt_to_phys(&self, virt: *const usize) -> Result<usize, Error> {
        let entry = self.get_l2_entry(virt).or(Err(Error::BadAddress))?;
        let entry_data = L2TableEntry::read_from(entry);

        if entry_data.is_mapped() {
            entry_data.phys()
        } else if entry_data.is_empty() {
            Err(Error::BadAddress)
        } else if entry_data.is_shared() {
            Err(Error::ShareViolation)
        } else {
            Err(Error::MemoryInUse)
        }
    }

    /// Perform `op` on the whole cache when `len` is large enough that the
    /// per-page, per-cache-line path would be slower than a single by-way
    /// pass, returning whether it did. Callers should fall back to per-page
    /// work when this returns false.
    ///
    /// Whole-cache Invalidate is deliberately not handled: it would discard
    /// other processes' dirty lines. Callers needing invalidate semantics on
    /// a large region should pass CleanAndInvalidate, which writes those lines back.
    pub fn try_whole_cache_flush(&self, len: usize, op: CacheOperation) -> bool {
        if len <= FULL_CACHE_FLUSH_THRESHOLD {
            return false;
        }
        use crate::platform::atsama5d2::cache;
        match op {
            CacheOperation::Clean => {
                cache::clean_cache_l1();
                cache::clean_cache_l2();
                true
            }
            CacheOperation::CleanAndInvalidate => {
                cache::clean_invalidate_cache_l1();
                cache::clean_invalidate_cache_l2();
                true
            }
            CacheOperation::Invalidate => false,
        }
    }

    /// Flush L1 and L2 caches.
    /// Start and size don't need to be aligned to anything.
    pub fn flush_cache(&self, mem: MemoryRange, op: CacheOperation) -> Result<(), Error> {
        if self.try_whole_cache_flush(mem.len(), op) {
            return Ok(());
        }

        // Align the start address at the page boundary
        let range_start = mem.as_ptr() as usize;
        let aligned_range_start = range_start & !(PAGE_SIZE - 1);
        let mut range_start_offset = range_start - aligned_range_start;

        let range_end = mem.as_ptr() as usize + mem.len();

        for page_start_virt in (aligned_range_start..range_end).step_by(PAGE_SIZE) {
            // End doesn't need to be aligned
            let page_end_virt = range_end.min(page_start_virt + PAGE_SIZE);
            let page_start_phys = self.virt_to_phys(page_start_virt as *const usize)?;
            let page_end_phys = page_start_phys + page_end_virt - page_start_virt;
            crate::platform::atsama5d2::cache::flush_cache_region_l1(
                (page_start_virt + range_start_offset) as u32,
                page_end_virt as u32,
                op,
            );
            crate::platform::atsama5d2::cache::flush_cache_region_l2(
                (page_start_phys + range_start_offset) as u32,
                page_end_phys as u32,
                op,
            );
            range_start_offset = 0;
        }

        Ok(())
    }

    /// Invalidate the cache for a single page of memory. Both virt and phys should be aligned.
    pub fn invalidate_page(&self, virt: *mut usize, phys: usize) {
        crate::platform::atsama5d2::cache::flush_cache_region_l1(
            virt as u32,
            virt as u32 + PAGE_SIZE as u32,
            CacheOperation::Invalidate,
        );
        crate::platform::atsama5d2::cache::flush_cache_region_l2(
            phys as u32,
            phys as u32 + PAGE_SIZE as u32,
            CacheOperation::Invalidate,
        );
    }

    /// Start of the first run of `needed` free (unmapped) pages in `[from, end)`,
    /// or `None`. One forward pass: each L1 descriptor is read once per section,
    /// each L2 entry at most once, and a section with no L2 table contributes its
    /// whole span with no L2 reads.
    pub fn find_free_run(&self, from: usize, end: usize, needed: usize) -> Option<usize> {
        let mut addr = from;
        let mut run_start = from;
        let mut run_len = 0;

        while addr < end {
            let vpn1 = VirtualAddress::new(addr as u32).translation_table_index();
            let section_end =
                (VirtualAddress::from_indices(vpn1 + 1, 0, 0).unwrap().as_u32() as usize).min(end);
            let l1 = unsafe { self.l1_entry_ptr(vpn1).read_volatile() };
            match l1.get_type() {
                // No L2 table: the whole rest of the section is free.
                TranslationTableType::Invalid => {
                    if run_len == 0 {
                        run_start = addr;
                    }
                    run_len += (section_end - addr) / PAGE_SIZE;
                    if run_len >= needed {
                        return Some(run_start);
                    }
                    addr = section_end;
                }
                TranslationTableType::Page => {
                    let l2_base = transparent_phys_to_virt(l1.get_addr().unwrap());
                    while addr < section_end {
                        let vpn2 = VirtualAddress::new(addr as u32).page_table_index();
                        if L2TableEntry::read_from(unsafe { l2_base.add(vpn2) }).is_empty() {
                            if run_len == 0 {
                                run_start = addr;
                            }
                            run_len += 1;
                            if run_len >= needed {
                                return Some(run_start);
                            }
                        } else {
                            run_len = 0;
                        }
                        addr += PAGE_SIZE;
                    }
                }
                // Section/supersection mapping: occupied, breaks the run.
                _ => {
                    run_len = 0;
                    addr = section_end;
                }
            }
        }
        None
    }

    /// Determine whether a virtual address has been mapped
    pub fn address_user_accessible(&self, virt: *const usize) -> bool {
        if let Ok(entry) = self.get_l2_entry(virt) {
            L2TableEntry::read_from(entry).is_user_accessible()
        } else {
            true
        }
    }

    /// Determine whether a virtual address has been mapped with an X flag
    pub fn address_executable(&self, virt: *const usize) -> bool {
        let Ok(entry) = self.get_l2_entry(virt) else {
            return false;
        };
        let entry_data = L2TableEntry::read_from(entry);
        if !entry_data.is_mapped() {
            return false;
        }
        entry_data.flags().map(|f| f.is_set(MemoryFlags::X)).unwrap_or(false)
    }

    /// Allocate a backing page for a page that is marked on-demand (allocated but unmapped).
    /// Can only work with the currently activated MemoryMapping (hence no &self)
    /// because it needs to be able to zero the new page.
    pub fn ensure_backed(mm: &mut MemoryManager, virt: *mut usize) -> Result<(), Error> {
        let this = Self::current();

        let entry = this.get_l2_entry(virt)?;
        let entry_data = L2TableEntry::read_from(entry);
        if entry_data.is_mapped() {
            return Ok(());
        }
        if entry_data.is_empty() {
            return Err(Error::BadAddress);
        }
        if !entry_data.is_on_demand() {
            return Err(Error::MemoryInUse);
        }
        this.allocate_backing_page(mm, virt, entry, entry_data)
    }

    /// Give an on-demand entry a freshly allocated, zeroed backing page. Takes
    /// `entry` and `entry_data` from a prior get_l2_entry so it adds no walk of
    /// its own; `entry_data` must already be on-demand.
    fn allocate_backing_page(
        &self,
        mm: &mut MemoryManager,
        virt: *mut usize,
        entry: *mut u32,
        entry_data: L2TableEntry,
    ) -> Result<(), Error> {
        let flags = entry_data.flags()?;
        // We need the page to be writeable to be able to zero it.
        // A read-only page full of 0s don't really make much sense anyway.
        if !flags.is_set(MemoryFlags::W) {
            return Err(Error::AccessDenied);
        }
        let (mut phys, mut zeroed) = mm.alloc_range(1, self.pid)?;
        if flags.is_set(MemoryFlags::PLAINTEXT)
            || flags.is_set(MemoryFlags::NO_CACHE)
            || flags.is_set(MemoryFlags::DEV)
        {
            phys = to_plaintext_phys_addr(phys);
            zeroed = false;
        }

        entry_data.with_phys(phys)?.to_mapped(virt)?.write_to(virt, entry);

        if !zeroed {
            let page_slice =
                unsafe { core::slice::from_raw_parts_mut(virt, PAGE_SIZE / core::mem::size_of::<usize>()) };
            page_slice.fill(0);
            self.flush_cache(
                unsafe { MemoryRange::new(virt as usize, PAGE_SIZE).unwrap() },
                xous::CacheOperation::Clean,
            )?;
            core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }

    #[cfg(any(not(feature = "production"), feature = "log-serial"))]
    fn print_small_page_range(
        vpn1: usize,
        first: usize,
        first_phys: usize,
        flags: u32,
        pages: usize,
        mut output: impl core::fmt::Write,
    ) {
        let flags_decoded = decode_flags(first_phys, flags);
        let first_virt = (vpn1 << 20) | (first << 12);
        let last = first + pages - 1;
        let last_virt = (vpn1 << 20) | (last << 12);
        let last_phys = first_phys + (pages - 1) * PAGE_SIZE;
        write!(output,
            "        - {first:02x} - {last:02x}  Small Page {first_virt:08x} -> {first_phys:08x} | flags: {:04x} | 0b{:08b} | ",
            flags & 0xfff,
            flags_decoded.bits(),
        ).ok();
        flags_decoded.print(|f| {
            write!(output, "{}", f).ok();
        });
        writeln!(output,).ok();
        if pages > 1 {
            write!(output,
                "                   Small Page {last_virt:08x} -> {last_phys:08x} | flags: {:04x} | 0b{:08b} | ",
                flags & 0xfff,
                flags_decoded.bits(),
            ).ok();
            flags_decoded.print(|f| {
                write!(output, "{}", f).ok();
            });
            writeln!(output).ok();
        }
    }

    #[cfg(any(not(feature = "production"), feature = "log-serial"))]
    fn print_l2_pagetable(vpn1: usize, table_addr: *mut u32, mut output: impl core::fmt::Write) {
        let l2_ptr = unsafe { PageTable::new_from_ptr(table_addr as _) };
        let l2_pt = l2_ptr.table();

        let mut no_valid_items = true;
        let mut contigous_small_pages = None;
        for (i, pt_desc) in l2_pt.iter().enumerate() {
            let virt_addr = (vpn1 << 20) | (i << 12);

            let phys_addr = pt_desc.get_addr().unwrap_or(PhysicalAddress::new(0));
            let flags = pt_desc.get_flags().expect("flags");

            if let Some((first, first_phys, first_flags)) = &contigous_small_pages {
                let pages = i - *first;
                if pt_desc.get_type() != PageTableType::SmallPage
                    || flags & 0xfff != *first_flags & 0xfff
                    || phys_addr.as_u32() as usize != *first_phys + pages * PAGE_SIZE
                {
                    Self::print_small_page_range(vpn1, *first, *first_phys, *first_flags, pages, &mut output);
                    contigous_small_pages = None;
                }
            }

            match pt_desc.get_type() {
                PageTableType::LargePage => {
                    writeln!(
                        output,
                        "        - {:02x} (64K) Large Page {:08x} -> {:08x}",
                        i, virt_addr, phys_addr
                    )
                    .ok();
                    no_valid_items = false;
                }
                PageTableType::SmallPage => {
                    if contigous_small_pages.is_none() {
                        contigous_small_pages = Some((i, phys_addr.as_u32() as usize, flags))
                    }
                    no_valid_items = false;
                }
                _ => {}
            }
        }
        if let Some((first, first_phys, first_flags)) = &contigous_small_pages {
            Self::print_small_page_range(
                vpn1,
                *first,
                *first_phys,
                *first_flags,
                PAGE_TABLE_SIZE - *first,
                &mut output,
            );
        }

        if no_valid_items {
            writeln!(output, "        - <no valid items>").ok();
        }
    }

    #[cfg(any(not(feature = "production"), feature = "log-serial"))]
    pub fn print_map(&self, mut output: impl core::fmt::Write) {
        writeln!(output, "    TTBR0: {:08x}", self.ttbr0.as_u32()).ok();
        let tt = TranslationTable::new(transparent_phys_to_virt(self.ttbr0) as _);

        for (i, tt_desc) in tt.table().iter().enumerate() {
            if !self.is_kernel() && i >= L1_USER_PAGE_TABLE_ENTRIES {
                break;
            }
            if let TranslationTableType::Invalid = tt_desc.get_type() {
                continue;
            }

            let phys_addr = tt_desc.get_addr().expect("addr");
            let virt_addr = i << 20;
            match tt_desc.get_type() {
                TranslationTableType::Page => {
                    let table_virt_addr = transparent_phys_to_virt(tt_desc.get_addr().unwrap());
                    writeln!(
                        output,
                        "    - {:03x} (1MB) {:08x} L2 page table @ {:08x} (v. {:08x?})",
                        i, virt_addr, phys_addr, table_virt_addr,
                    )
                    .ok();
                    Self::print_l2_pagetable(i, table_virt_addr, &mut output);
                }
                TranslationTableType::Section => {
                    writeln!(output, "    - {:03x} (1MB)  section {:08x} -> {:08x}", i, virt_addr, phys_addr)
                        .ok();
                }
                TranslationTableType::Supersection => {
                    writeln!(
                        output,
                        "    - {:03x} (16MB) supersection {:08x} -> {:08x}",
                        i, virt_addr, phys_addr
                    )
                    .ok();
                }

                _ => (),
            }
        }
    }

    #[cfg(any(not(feature = "production"), feature = "log-serial"))]
    pub fn check_consistency(&self, mm: &MemoryManager, mut output: impl core::fmt::Write) {
        for page in 0..4 {
            if !self.is_kernel() && page >= L1_USER_PAGE_TABLE_PAGES {
                break;
            }
            let owner = mm.page_owner(self.ttbr0.as_u32() as usize + page * PAGE_SIZE);
            if owner != Some(self.pid) {
                writeln!(output, "[!] Incorrect owner of L1 page {page}: {owner:?}").ok();
            }
        }
        let tt = TranslationTable::new(transparent_phys_to_virt(self.ttbr0) as _);

        for (i, tt_desc) in tt.table().iter().enumerate() {
            if !self.is_kernel() && i >= L1_USER_PAGE_TABLE_ENTRIES {
                break;
            }
            match tt_desc.get_type() {
                TranslationTableType::Invalid => continue,
                TranslationTableType::Page => {
                    self.check_consistency_l2(mm, i, tt_desc.get_addr().unwrap(), &mut output)
                }
                TranslationTableType::Section => { /* No checks on Sections yet */ }
                _ => {
                    writeln!(output, "[!] Unexpected L1 descriptor: {tt_desc:x?}").ok();
                }
            }
        }
    }

    #[cfg(any(not(feature = "production"), feature = "log-serial"))]
    fn check_consistency_l2(
        &self,
        mm: &MemoryManager,
        vpn1: usize,
        l2_phys: PhysicalAddress,
        mut output: impl core::fmt::Write,
    ) {
        let owner = mm.page_owner(l2_phys.as_u32() as usize);
        if owner != Some(self.pid) {
            writeln!(output, "[!] Incorrect owner of L2 page {vpn1:03x} (phys: {l2_phys:08x}): {owner:?}")
                .ok();
        }
        let l2_ptr = unsafe { PageTable::new_from_ptr(transparent_phys_to_virt(l2_phys) as _) };
        let l2_pt = l2_ptr.table();

        for (i, pt_desc) in l2_pt.iter().enumerate() {
            let virt_addr = (vpn1 << 20) | (i << 12);
            if let PageTableType::Invalid = pt_desc.get_type() {
                continue;
            }
            if (MEMORY_MIRROR_AREA_VIRT..MEMORY_MIRROR_AREA_VIRT + 0x4000000).contains(&virt_addr) {
                // Mirrored area will always have a different ownership, by design.
                continue;
            }
            let phys_addr = pt_desc.get_addr().expect("addr").as_u32() as usize;

            const PID1: Option<PID> = PID::new(1);
            const SPECIAL_CASES: &[(usize, usize, Option<PID>)] = &[
                (KERNEL_LOAD_OFFSET, NUM_KERNEL_PAGES_MAX, PID1),
                (KERNEL_STACK_BOTTOM - KERNEL_STACK_PAGE_COUNT * PAGE_SIZE, KERNEL_STACK_PAGE_COUNT, PID1),
                (IRQ_STACK_BOTTOM - IRQ_STACK_PAGE_COUNT * PAGE_SIZE, IRQ_STACK_PAGE_COUNT, PID1),
                (
                    EXCEPTION_STACK_BOTTOM - EXCEPTION_STACK_PAGE_COUNT * PAGE_SIZE,
                    EXCEPTION_STACK_PAGE_COUNT,
                    PID1,
                ),
                (ALLOCATION_TRACKER_OFFSET, ALLOCATION_TRACKER_PAGES_MAX, PID1),
            ];
            let mut expected_owner = Some(self.pid);
            for (start, pages, owner) in SPECIAL_CASES {
                if virt_addr >= *start && virt_addr < *start + *pages * PAGE_SIZE {
                    expected_owner = *owner
                }
            }
            for phys_start in &SHARED_PERIPHERALS {
                if phys_addr >= *phys_start && phys_addr < *phys_start + 4 * PAGE_SIZE {
                    expected_owner = None;
                }
            }
            let owner = mm.page_owner(phys_addr);
            if owner != expected_owner {
                writeln!(output,
                    "[!] Incorrect owner of regular page virt:{virt_addr:08x} phys:{phys_addr:08x}: {owner:?} (expected: {expected_owner:?})"
                ).ok();
            }
        }
    }
}

impl L2TableEntry {
    const MAP_USER_FLAG: usize = 1 << 2;
    const UNMAPPED_FLAGS_OFFSET: usize = 3;

    /// Read entry from the hardware L2 page table
    fn read_from(pt_entry: *const u32) -> Self {
        let value = unsafe { pt_entry.read_volatile() };
        if value == 0 {
            L2TableEntry::Empty
        } else if value & 3 == 0 {
            L2TableEntry::Unmapped(value as usize)
        } else {
            L2TableEntry::Mapped(PageTableDescriptor::from_u32(value))
        }
    }

    /// Write entry to the hardware L2 page table.
    /// Does all necessary cache and TLB synchronizations.
    fn write_to(&self, virt_addr: *mut usize, pt_entry: *mut u32) {
        let value = match self {
            L2TableEntry::Empty => 0,
            L2TableEntry::Mapped(d) => d.as_u32(),
            L2TableEntry::Unmapped(d) => *d as u32,
        };
        unsafe { pt_entry.write_volatile(value) };
        if self.is_empty() {
            // Removing a mapping: order the write, invalidate the stale TLB entry.
            flush_tlb_entry(virt_addr);
        } else {
            // New mapping: order the (cacheable) descriptor write before the
            // walker can fetch it. No prior entry, so nothing to invalidate.
            unsafe { core::arch::asm!("dsb") };
        }
    }

    /// Create a new on-demand entry
    fn new_on_demand(phys: usize, flags: MemoryFlags, map_user: bool) -> Result<Self, Error> {
        assert_eq!(phys & (PAGE_SIZE - 1), 0);
        // This will create an invalid L2 entry with both bottom bits set to 0
        let val = phys
            | (flags.bits() << Self::UNMAPPED_FLAGS_OFFSET)
            | if map_user { Self::MAP_USER_FLAG } else { 0 };
        Ok(Self::Unmapped(val))
    }

    /// Create a new physically backed entry
    fn new_mapped(
        virt: *const usize,
        phys: usize,
        flags: MemoryFlags,
        map_user: bool,
    ) -> Result<Self, Error> {
        if phys & (PAGE_SIZE - 1) != 0 {
            return Err(Error::BadAlignment);
        }

        let mut small_page_flags = u32::from(SMALL_PAGE_FLAGS::VALID::Enable);

        // Disable execution if the X flag is not set
        if !flags.is_set(MemoryFlags::X) {
            small_page_flags |= u32::from(SMALL_PAGE_FLAGS::XN::Enable);
        }

        // Enable cache for non-device / cacheable pages
        if !flags.is_set(MemoryFlags::DEV) && !flags.is_set(MemoryFlags::NO_CACHE) {
            small_page_flags |= u32::from(SMALL_PAGE_FLAGS::C::Enable); // Cacheable
        }

        if flags.is_set(MemoryFlags::DEV) {
            small_page_flags |= u32::from(SMALL_PAGE_FLAGS::B::Enable); // Mark as device memory through TRE
        }

        // See ARM "ARM" Table B3-8 VMSAv7 MMU access permissions.
        if !flags.is_set(MemoryFlags::W) {
            small_page_flags |= u32::from(SMALL_PAGE_FLAGS::AP2::Enable);
        }
        if map_user {
            small_page_flags |= u32::from(SMALL_PAGE_FLAGS::AP::FullAccess)
        } else {
            small_page_flags |= u32::from(SMALL_PAGE_FLAGS::AP::PrivAccess)
        }

        // Pages in the lower TT (TTBR0) are always non-global, while TTBR1 entries are always global.
        if (virt as usize) < TTBR1_SPLIT {
            small_page_flags |= u32::from(SMALL_PAGE_FLAGS::NG::Enable);
        }

        let new_entry = PageTableDescriptor::new(
            PageTableType::SmallPage,
            PhysicalAddress::new(phys as u32),
            small_page_flags,
        )
        .expect("new l2 entry");
        Ok(Self::Mapped(new_entry))
    }

    /// Physical address of the entry. May be 0 if the page is marked on-demand.
    /// Returns an error if the entry itself is empty.
    fn phys(&self) -> Result<usize, Error> {
        match self {
            L2TableEntry::Empty => Err(Error::BadAddress),
            L2TableEntry::Mapped(m) => Ok(m.get_addr().unwrap().as_u32() as usize),
            L2TableEntry::Unmapped(u) => Ok(*u & !(PAGE_SIZE - 1)),
        }
    }

    /// Memory flags of the entry. Works for mapped, on-demand and shared pages.
    /// Returns an error if the entry itself is empty.
    fn flags(&self) -> Result<MemoryFlags, Error> {
        match self {
            L2TableEntry::Empty => Err(Error::BadAddress),
            L2TableEntry::Unmapped(u) => Ok(MemoryFlags::from_bits(*u >> Self::UNMAPPED_FLAGS_OFFSET)),
            L2TableEntry::Mapped(m) => {
                Ok(decode_flags(m.get_addr().unwrap().as_u32() as usize, m.get_flags().unwrap()))
            }
        }
    }

    /// Convert entry to a mapped one. If it is already mapped, this is a noop.
    /// Returns an error if the entry itself is empty.
    fn to_mapped(self, virt: *const usize) -> Result<Self, Error> {
        match self {
            L2TableEntry::Empty => Err(Error::BadAddress),
            L2TableEntry::Mapped(m) => Ok(L2TableEntry::Mapped(m)),
            L2TableEntry::Unmapped(u) => {
                Self::new_mapped(virt, self.phys()?, self.flags()?, (u & Self::MAP_USER_FLAG) != 0)
            }
        }
    }

    /// Convert entry to an unmapped one (e.g. from mapped to shared).
    /// If it is already mapped, this is a noop.
    /// Returns an error if the entry itself is empty.
    fn to_unmapped(self) -> Result<Self, Error> {
        match self {
            L2TableEntry::Empty => Err(Error::BadAddress),
            L2TableEntry::Mapped(m) => {
                let flags: InMemoryRegister<u32, SMALL_PAGE_FLAGS::Register> =
                    InMemoryRegister::new(m.get_flags().unwrap());
                Self::new_on_demand(
                    self.phys()?,
                    self.flags()?,
                    flags.matches_all(SMALL_PAGE_FLAGS::AP::FullAccess),
                )
            }
            L2TableEntry::Unmapped(u) => Ok(L2TableEntry::Unmapped(u)),
        }
    }

    /// Does the entry correspond to a currently shared page
    fn is_shared(&self) -> bool {
        if let L2TableEntry::Unmapped(u) = self {
            *u & !(PAGE_SIZE - 1) != 0
        } else {
            false
        }
    }

    /// Is the entry a currently unbacked on-demand page
    fn is_on_demand(&self) -> bool {
        if let L2TableEntry::Unmapped(u) = self {
            *u & !(PAGE_SIZE - 1) == 0
        } else {
            false
        }
    }

    /// Is the entry unallocated
    fn is_empty(&self) -> bool { matches!(self, L2TableEntry::Empty) }

    /// Is the entry backed by a physical page
    fn is_mapped(&self) -> bool { matches!(self, L2TableEntry::Mapped(_)) }

    /// Is the entry valid and accessible from userspace (not necessarily mapped)
    fn is_user_accessible(&self) -> bool {
        match self {
            L2TableEntry::Empty => false,
            L2TableEntry::Mapped(m) => {
                m.get_flags().unwrap() & u32::from(SMALL_PAGE_FLAGS::AP::FullAccess)
                    == u32::from(SMALL_PAGE_FLAGS::AP::FullAccess)
            }
            L2TableEntry::Unmapped(um) => um & Self::MAP_USER_FLAG != 0,
        }
    }

    /// Strip the W flag from the entry, whether mapped or unmapped
    fn to_immutable(self) -> Result<Self, Error> {
        match self {
            L2TableEntry::Empty => Err(Error::BadAddress),
            L2TableEntry::Mapped(m) => Ok(L2TableEntry::Mapped(PageTableDescriptor::from_u32(
                m.as_u32() | u32::from(SMALL_PAGE_FLAGS::AP2::Enable),
            ))),
            L2TableEntry::Unmapped(u) => {
                Ok(L2TableEntry::Unmapped(u & !(MemoryFlags::W.bits() << Self::UNMAPPED_FLAGS_OFFSET)))
            }
        }
    }

    /// Change the physical address of the entry
    fn with_phys(self, phys: usize) -> Result<Self, Error> {
        assert_eq!(phys & (PAGE_SIZE - 1), 0);
        match self {
            L2TableEntry::Empty => Err(Error::BadAddress),
            L2TableEntry::Mapped(m) => Ok(L2TableEntry::Mapped(
                PageTableDescriptor::new(
                    PageTableType::SmallPage,
                    PhysicalAddress::new(phys as u32),
                    m.get_flags().unwrap(),
                )
                .unwrap(),
            )),
            L2TableEntry::Unmapped(u) => Ok(L2TableEntry::Unmapped(u & (PAGE_SIZE - 1) | phys)),
        }
    }
}

fn decode_flags(phys: usize, flags: u32) -> MemoryFlags {
    let flags: InMemoryRegister<u32, SMALL_PAGE_FLAGS::Register> = InMemoryRegister::new(flags);
    let mut flags_out = MemoryFlags::empty();
    if flags.read(SMALL_PAGE_FLAGS::AP2) == 0 {
        flags_out |= MemoryFlags::W;
    }
    if flags.read(SMALL_PAGE_FLAGS::XN) == 0 {
        flags_out |= MemoryFlags::X;
    }
    if flags.read(SMALL_PAGE_FLAGS::C) == 0 {
        flags_out |= MemoryFlags::NO_CACHE;
    }
    if flags.read(SMALL_PAGE_FLAGS::B) != 0 {
        flags_out |= MemoryFlags::DEV;
    }
    if is_address_in_plaintext_dram(phys) {
        flags_out |= MemoryFlags::PLAINTEXT;
    }
    flags_out
}

fn transparent_phys_to_virt(phys: PhysicalAddress) -> *mut u32 {
    let phys = phys.as_u32() as usize;
    assert!(
        (ENCRYPTED_DRAM_BASE..ENCRYPTED_DRAM_END).contains(&phys),
        "{phys:08x} not in encrypted DRAM area",
    );
    (phys - ENCRYPTED_DRAM_BASE + MAPPED_PHYSICAL_RAM) as _
}
