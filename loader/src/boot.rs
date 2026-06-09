// SPDX-FileCopyrightText: 2022 Sean Cross <sean@xobs.io>
// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use core::mem;

use armv7::structures::paging::{
    PageTable as L2PageTable, PageTableDescriptor, PageTableMemory, PageTableType, TranslationTable,
    TranslationTableDescriptor, TranslationTableMemory, TranslationTableType, PAGE_TABLE_FLAGS,
    SMALL_PAGE_FLAGS,
};
use armv7::{PhysicalAddress, VirtualAddress};
use keyos::{
    is_address_in_plaintext_dram, to_encrypted_phys_addr, ALLOCATION_TRACKER_OFFSET, BOOT_SPLASH_FB,
    BOOT_SPLASH_PAGES, BOOT_SPLASH_PHYS_ADDR, ENCRYPTED_DRAM_BASE, ENCRYPTED_DRAM_END,
    ICM_KERNEL_DESC_AREA_ADDR, ICM_KERNEL_HASH_AREA_ADDR, L1_USER_PAGE_TABLE_ENTRIES, MAPPED_PHYSICAL_RAM,
    RAM_PAGES, RAM_SIZE,
};

use super::consts::{
    FLG_DEV, FLG_GUARD, FLG_NO_CACHE, FLG_R, FLG_U, FLG_VALID, FLG_W, FLG_X, KERNEL_ARGUMENT_OFFSET,
};
use crate::{additional_regions, bzero, BootConfig, PAGE_SIZE};

const DEBUG_PAGE_MAPPING: bool = false;
macro_rules! dprint {
    ($($args:tt)*) => ({
        if DEBUG_PAGE_MAPPING {
            crate::print!($($args)*)
        }
    });
}
macro_rules! dprintln {
    ($($args:tt)*) => ({
        if DEBUG_PAGE_MAPPING {
            crate::println!($($args)*)
        }
    });
}

impl BootConfig {
    pub fn get_top(&self) -> *mut usize {
        (ENCRYPTED_DRAM_END - self.rpt_size_bytes - self.extra_pages * PAGE_SIZE) as *mut usize
    }

    /// Zero-alloc a new page, mark it as owned by PID1, and return it.
    /// Decrement the `next_page_offset` (npo) variable by one page.
    pub fn alloc(&mut self) -> *mut usize {
        self.extra_pages += 1;
        assert!(self.extra_pages < RAM_PAGES - self.rpt_size_bytes / PAGE_SIZE);
        let pg = self.get_top();
        unsafe {
            // Grab the page address and zero it out
            bzero(pg as *mut usize, pg.add(PAGE_SIZE / mem::size_of::<usize>()) as *mut usize);
        }
        // Mark this page as in-use by the kernel
        self.runtime_page_tracker[RAM_PAGES - self.extra_pages - self.rpt_size_bytes / PAGE_SIZE] = 1;

        dprintln!("Allocated a physical page: {:08x}", pg as usize);

        // Return the address
        pg as *mut usize
    }

    /// Allocates four 4K pages for L1 translation table
    /// May waste some pages as dummy pages due to alignment requirements.
    /// Sometimes there are none, but could be up to 3 pages wasted.
    pub fn alloc_l1_page_table(&mut self) -> *mut usize {
        // ARMv7A Level 1 Translation Table is required to be aligned at 16K boundary
        const ALIGNMENT_16K: usize = 16 * 1024;

        // It should take no more than 4 tries to get to the next 16K-aligned 4K sized page
        let mut num_alloc_pages = 0;
        for _ in 0..4 {
            let mut allocated_page_ptr = self.alloc();
            num_alloc_pages += 1;
            let is_aligned = allocated_page_ptr as usize & (ALIGNMENT_16K - 1) == 0;
            self.mark_as_owned(allocated_page_ptr as usize);

            if is_aligned {
                return if num_alloc_pages != 4 {
                    dprintln!(
                        "Allocated a dummy page (aligned but not enough pages allocated yet): {:08x}",
                        allocated_page_ptr as usize
                    );

                    // Allocate 4 more pages for a whole L1 translation table
                    for _ in 0..4 {
                        dprintln!(
                            "Allocated a page {:08x} for PID 1 L1 page table",
                            allocated_page_ptr as usize,
                        );
                        allocated_page_ptr = self.alloc();
                        self.mark_as_owned(allocated_page_ptr as usize);
                    }

                    dprintln!("Allocated a L1 page table at {:08x}", allocated_page_ptr as usize);

                    allocated_page_ptr
                } else {
                    allocated_page_ptr
                };
            } else {
                dprintln!("Allocated a dummy page for alignment: {:08x}", allocated_page_ptr as usize);
            }
        }

        unreachable!("Couldn't allocate a 16K-aligned page for L1 page table base")
    }

    pub fn mark_as_owned(&mut self, addr: usize) {
        let addr = if is_address_in_plaintext_dram(addr) { to_encrypted_phys_addr(addr) } else { addr };
        dprintln!("Marking {:08x} as owned by the kernel", addr);

        // First, check to see if the region is in RAM,
        if addr >= ENCRYPTED_DRAM_BASE && addr < ENCRYPTED_DRAM_END {
            // Mark this page as in-use by the PID
            self.runtime_page_tracker[(addr - ENCRYPTED_DRAM_BASE) / PAGE_SIZE] = 1;
            return;
        }
        // The region isn't in RAM, so check the other memory regions.
        let mut rpt_offset = RAM_PAGES;

        for range in additional_regions() {
            let rlen = range.end - range.start;
            if range.contains(&addr) {
                self.runtime_page_tracker[rpt_offset + (addr - range.start) / PAGE_SIZE] = 1;
                return;
            }
            rpt_offset += rlen / PAGE_SIZE;
        }
        panic!("Tried to change region {:08x} that isn't in defined memory!", addr);
    }

    /// Map the given page to the specified process table.  If necessary,
    /// allocate a new page.
    ///
    /// # Panics
    ///
    /// * If you try to map a page twice
    pub fn map_page(
        &mut self,
        translation_table: *mut TranslationTableMemory,
        phys: usize,
        virt: usize,
        flags: usize,
    ) {
        dprintln!("PageTable: {:p} {:08x}", translation_table, translation_table as usize);
        dprint!("MAP: p0x{:08x} -> v0x{:08x} ", phys, virt);
        print_flags(flags);
        dprintln!();

        if flags & FLG_X != 0 && flags & FLG_W != 0 {
            panic!("Tried to map RWX page! phys: 0x{:08x}, virt: 0x{:08x}", phys, virt);
        }

        self.mark_as_owned(phys);

        let v = VirtualAddress::new(virt as u32);
        let vpn1 = v.translation_table_index();
        let vpn2 = v.page_table_index();

        let p = phys & !(0xfff);
        let ppn2 = (p >> 12) & 0xff;

        assert!(vpn1 < 4096);
        assert!(vpn2 < 256);
        assert!(ppn2 < 256);

        dprintln!("vpn1: {:04x}, vpn2: {:02x}, ppn2: {:08x}, phys frame addr: {:08x}", vpn1, vpn2, ppn2, p);

        let mut tt = TranslationTable::new(translation_table);
        let tt = unsafe { tt.table_mut() };

        // Allocate a new level 1 translation table entry if one doesn't exist.
        dprintln!("tt[{:08x}] = {:032b}", vpn1, tt[vpn1]);

        if tt[vpn1].get_type() == TranslationTableType::Invalid {
            dprintln!("Previously unmapped L1 entry");

            // Pack four 1KiB L2 tables into one 4KiB page by backing the whole
            // aligned 4-section group, rather than burning a page per section.
            // Keeping every group all-Invalid or all-Page lets the kernel grow
            // L2 tables without checking siblings; the only map_section region
            // (the RAM window) is 4MiB-group-aligned, so a group is never
            // part-section, part-page.
            const L2_TABLE_BYTES: u32 = 256 * 4;
            let na = self.alloc();
            self.mark_as_owned(na as usize);
            let page_phys = PhysicalAddress::from_ptr(na).as_u32();
            let entry_flags =
                u32::from(PAGE_TABLE_FLAGS::VALID::Enable) | u32::from(PAGE_TABLE_FLAGS::DOMAIN.val(0xf));
            let group_base = vpn1 & !0b11;
            for k in 0..4u32 {
                let sub_table = PhysicalAddress::new(page_phys + k * L2_TABLE_BYTES);
                let descriptor =
                    TranslationTableDescriptor::new(TranslationTableType::Page, sub_table, entry_flags)
                        .expect("tt descriptor");
                tt[group_base + k as usize] = descriptor;
            }
            dprintln!("new tt[{:08x}] = {:032b}", vpn1, tt[vpn1]);
        }

        let existing_entry = tt[vpn1];
        dprintln!("existing tt[{:08x}] = {:032b}", vpn1, existing_entry);
        match existing_entry.get_type() {
            TranslationTableType::Page => {
                let l2_phys_addr = existing_entry.get_addr().expect("invalid l1 entry");
                let ptr: *mut PageTableMemory = l2_phys_addr.as_mut_ptr();
                let mut l2_pt = unsafe { L2PageTable::new_from_ptr(ptr) };
                let l2_pt = unsafe { l2_pt.table_mut() };

                dprintln!("l2 ptr: {:p}", l2_pt);

                let existing_l2_entry = l2_pt[vpn2];

                dprintln!("({:08x}) l2_pt[{:08x}] = {:032b}", l2_phys_addr, vpn2, existing_l2_entry);

                if existing_l2_entry.get_type() != PageTableType::Invalid {
                    let mapped_addr =
                        existing_l2_entry.get_addr().expect("invalid l2 entry").as_u32() as usize;
                    panic!(
                        "Page {:08x} was already allocated to {:08x}, so cannot map to {:08x}!",
                        virt, mapped_addr, phys
                    );
                }

                // Map the L2 entry
                let mut small_page_flags = 0;
                let is_valid = flags & FLG_VALID != 0;
                if is_valid {
                    small_page_flags |= u32::from(SMALL_PAGE_FLAGS::VALID::Enable);

                    if flags & FLG_X == 0 {
                        small_page_flags |= u32::from(SMALL_PAGE_FLAGS::XN::Enable);
                    }
                }

                let is_user = flags & FLG_U != 0;
                let is_read_only = (flags & FLG_W) == 0;
                let is_guard = flags & FLG_GUARD != 0;
                let is_dev = flags & FLG_DEV != 0;
                let is_no_cache = flags & FLG_NO_CACHE != 0;
                let is_global = vpn1 >= L1_USER_PAGE_TABLE_ENTRIES;

                let access = if is_guard {
                    SMALL_PAGE_FLAGS::AP::NoAccess
                } else if is_user {
                    if is_read_only {
                        SMALL_PAGE_FLAGS::AP::UnprivReadOnly
                    } else {
                        SMALL_PAGE_FLAGS::AP::FullAccess
                    }
                } else {
                    SMALL_PAGE_FLAGS::AP::PrivAccess
                };

                small_page_flags |= u32::from(access);

                if is_read_only && !is_guard {
                    small_page_flags |= u32::from(SMALL_PAGE_FLAGS::AP2::Enable);
                }

                // Enable cache for non-device / cacheable pages
                if !is_dev && !is_no_cache {
                    small_page_flags |= u32::from(SMALL_PAGE_FLAGS::C::Enable);
                }
                if is_dev {
                    // Set as "Device" through the TRE
                    small_page_flags |= u32::from(SMALL_PAGE_FLAGS::B::Enable);
                }
                if !is_global {
                    small_page_flags |= u32::from(SMALL_PAGE_FLAGS::NG::Enable);
                }

                let new_entry = PageTableDescriptor::new(
                    PageTableType::SmallPage,
                    PhysicalAddress::new(p as u32),
                    small_page_flags,
                )
                .expect("new l2 entry");
                l2_pt[vpn2] = new_entry;
                dprintln!("new ({:08x}) l2_pt[{:08x}] = {:032b}", l2_phys_addr, vpn2, l2_pt[vpn2]);
            }

            _ => panic!("Invalid translation table entry type: {:?}", existing_entry.get_type()),
        }
    }

    pub fn map_section(
        &mut self,
        translation_table: *mut TranslationTableMemory,
        phys: usize,
        virt: usize,
        flags: usize,
    ) {
        dprintln!("PageTable: {:p} {:08x}", translation_table, translation_table as usize);
        dprint!("MAP Section: p0x{:08x} -> v0x{:08x} ", phys, virt);
        print_flags(flags);
        dprintln!();

        assert_eq!(virt & 0xfffff, 0);
        assert_eq!(phys & 0xfffff, 0);
        let v = VirtualAddress::new(virt as u32);
        let vpn1 = v.translation_table_index();
        assert!(vpn1 < 4096);

        let mut tt = TranslationTable::new(translation_table);
        let tt = unsafe { tt.table_mut() };

        // Allocate a new level 1 translation table entry if one doesn't exist.
        dprintln!("tt[{:08x}] = {:032b}", vpn1, tt[vpn1]);

        assert_eq!(tt[vpn1].get_type(), TranslationTableType::Invalid, "Address already mapped");

        let is_user = flags & FLG_U != 0;
        let is_read_only = (flags & FLG_W) == 0;
        let is_guard = flags & FLG_GUARD != 0;
        let is_dev = flags & FLG_DEV != 0;
        let is_no_cache = flags & FLG_NO_CACHE != 0;
        let is_global = vpn1 >= L1_USER_PAGE_TABLE_ENTRIES;

        // Lots of manual bitshifts below, because sections are not supported
        // by the armv7 crate we're using.
        let mut entry_flags =
            (1 << 1) // Section
            | (0xf << 5) // Domain: 0xf
            ;

        let ap01 = if is_guard {
            0b00 // No access
        } else if is_user {
            if is_read_only {
                0b01 // UnprivReadOnly
            } else {
                0b11 // full access
            }
        } else {
            0b01 // Private only access
        };

        entry_flags |= ap01 << 10;

        if is_read_only && !is_guard {
            entry_flags |= 1 << 15; // AP[2]
        }

        // Enable cache for non-device / cacheable pages
        if !is_dev && !is_no_cache {
            entry_flags |= 1 << 3; // C
        }
        if is_dev {
            // Set as "Device" through the TRE
            entry_flags |= 1 << 2; // B
        }
        if !is_global {
            entry_flags |= 1 << 17; // NG
        }

        let descriptor = TranslationTableDescriptor::new(
            TranslationTableType::Section,
            PhysicalAddress::new(phys as u32),
            entry_flags,
        )
        .expect("tt descriptor");
        dprintln!("New TT descriptor: {:032b}", descriptor);
        tt[vpn1] = descriptor;
    }
}

pub fn map_runtime_page_tracker(cfg: &mut BootConfig) {
    let tt = cfg.pid1.ttbr0 as *mut TranslationTableMemory;

    for addr in (0..cfg.rpt_size_bytes).step_by(PAGE_SIZE) {
        let phys = cfg.runtime_page_tracker.as_ptr() as usize + addr;
        cfg.map_page(tt, phys, ALLOCATION_TRACKER_OFFSET + addr, FLG_R | FLG_W | FLG_VALID);
    }
}
pub fn map_arguments(cfg: &mut BootConfig) {
    let tt = cfg.pid1.ttbr0 as *mut TranslationTableMemory;
    for addr in (0..cfg.args.total_size()).step_by(PAGE_SIZE) {
        cfg.map_page(tt, cfg.args.base as usize + addr, KERNEL_ARGUMENT_OFFSET + addr, FLG_R | FLG_VALID);
    }
}

pub fn map_icm_regions(cfg: &mut BootConfig, kernel_text_start: usize, kernel_text_size: usize) {
    let tt = cfg.pid1.ttbr0 as *mut TranslationTableMemory;

    let phys = cfg.alloc();
    cfg.map_page(tt, phys as _, ICM_KERNEL_DESC_AREA_ADDR, FLG_R | FLG_W | FLG_VALID);

    // Put the kernel code offset and size on the page.
    // The kernel will use them to construct a proper ICM descriptor to monitor this region.
    unsafe {
        phys.write_volatile(kernel_text_start);
        phys.add(1).write_volatile(kernel_text_size);
    }

    let phys = cfg.alloc();
    cfg.map_page(tt, phys as _, ICM_KERNEL_HASH_AREA_ADDR, FLG_R | FLG_NO_CACHE | FLG_VALID);
}

#[allow(clippy::too_many_arguments)]
pub fn map_peripherals(cfg: &mut BootConfig) {
    let tt = cfg.pid1.ttbr0 as *mut TranslationTableMemory;

    const PERIPHERALS: &[(usize, usize, usize)] = &[
        (keyos::UART_ADDR, utralib::HW_UART1_BASE, 4),
        (keyos::TRNG_KERNEL_ADDR, utralib::HW_TRNG_BASE, 4),
        (keyos::L2CC_KERNEL_ADDR, utralib::HW_L2CC_BASE, 4),
        (keyos::AIC_KERNEL_ADDR, utralib::HW_AIC_BASE, 4),
        (keyos::SAIC_KERNEL_ADDR, utralib::HW_SAIC_BASE, 4),
        (keyos::SECURAM_KERNEL_ADDR, atsama5d27::securam::HW_SECURAM_BASE, 4),
        (keyos::RSTC_KERNEL_ADDR, utralib::HW_RSTC_BASE, 1),
        (keyos::RXLP_KERNEL_ADDR, utralib::HW_RXLP_BASE, 4),
        (keyos::SFC_KERNEL_ADDR, utralib::HW_SFC_BASE, 4),
        (keyos::ICM_KERNEL_ADDR, utralib::HW_ICM_BASE, 4),
    ];

    for (virt_start, phys_start, num_pages) in PERIPHERALS {
        for page in 0..*num_pages {
            let virt = (virt_start + (page * PAGE_SIZE)) & !(PAGE_SIZE - 1);
            let phys = (phys_start + (page * PAGE_SIZE)) & !(PAGE_SIZE - 1);
            cfg.map_page(tt, phys, virt, FLG_VALID | FLG_R | FLG_W | FLG_DEV);
        }
    }
}

pub fn map_physical_ram(cfg: &mut BootConfig) {
    let tt = cfg.pid1.ttbr0 as *mut TranslationTableMemory;
    for section in (0..RAM_SIZE).step_by(0x100000) {
        // Normal Write-Back Write-Allocate (C:1, B:0), non-shareable, so the
        // kernel's page-table accesses through this window cache and stay
        // coherent with the cacheable MMU table walks.
        cfg.map_section(
            tt,
            ENCRYPTED_DRAM_BASE + section,
            MAPPED_PHYSICAL_RAM + section,
            FLG_VALID | FLG_R | FLG_W,
        );
    }
}

pub fn map_boot_splash_screen(cfg: &mut BootConfig) {
    let tt = cfg.pid1.ttbr0 as *mut TranslationTableMemory;
    for page in 0..BOOT_SPLASH_PAGES {
        let virt = BOOT_SPLASH_FB + page * PAGE_SIZE;
        let phys = BOOT_SPLASH_PHYS_ADDR + page * PAGE_SIZE;
        cfg.map_page(tt, phys, virt, FLG_VALID | FLG_R | FLG_W | FLG_U);
    }
}

fn print_flags(flags: usize) {
    if flags & FLG_R != 0 {
        dprint!("R");
    }
    if flags & FLG_W != 0 {
        dprint!("W");
    }
    if flags & FLG_X != 0 {
        dprint!("X");
    }
    if flags & FLG_VALID != 0 {
        dprint!("V");
    }
    if flags & FLG_U != 0 {
        dprint!("U");
    }
    if flags & FLG_GUARD != 0 {
        dprint!("Gu");
    }
}
