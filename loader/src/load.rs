// SPDX-FileCopyrightText: 2022 Sean Cross <sean@xobs.io>
// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use core::mem;

use armv7::structures::paging::TranslationTableMemory;
use keyos::THREAD_CONTEXT_AREA;

use super::consts::{
    FLG_DEV, FLG_NO_CACHE, FLG_R, FLG_VALID, FLG_W, FLG_X, KERNEL_LOAD_OFFSET, KERNEL_MODE_STACK_BOTTOM,
    KERNEL_MODE_STACK_PAGE_COUNT, PID1_IRQ_HANDLER_STACK_BOTTOM, PID1_IRQ_HANDLER_STACK_PAGE_COUNT,
    PID1_STACK_BOTTOM, PID1_STACK_PAGE_COUNT,
};
use crate::{bzero, memcpy, println, BootConfig, PAGE_SIZE, VDBG};

/// This describes the kernel as well as initially-loaded processes
#[repr(C)]
pub struct ProgramDescription {
    /// Physical source address of this program in RAM (i.e. SPI flash).
    /// The image is assumed to contain a text section followed immediately
    /// by a data section.
    pub load_offset: u32,

    /// Start of the virtual address where the .text section will go.
    /// This section will be marked non-writable, executable.
    pub text_offset: u32,

    /// How many bytes of data to load from the source to the target
    pub text_size: u32,

    /// Start of the virtual address of .data and .bss section in RAM.
    /// This will simply allocate this memory and mark it "read-write"
    /// without actually copying any data.
    pub data_offset: u32,

    /// Size of the .data section, in bytes..  This many bytes will
    /// be allocated for the data section.
    pub data_size: u32,

    /// Size of the .bss section, in bytes.
    pub bss_size: u32,

    /// Virtual address entry point.
    pub entrypoint: u32,
}

impl ProgramDescription {
    pub fn copy(&self, cfg: &mut BootConfig) {
        // TEXT SECTION
        // Round it off to a page boundary
        let load_size_rounded = (self.text_size as usize + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        cfg.extra_pages += load_size_rounded / PAGE_SIZE;
        let top = cfg.get_top();
        println!("\n\nKernel top: {:x}, extra_pages: {:x}", top as u32, cfg.extra_pages);
        unsafe {
            // Copy the program to the target address, rounding it off to the load size.
            let src_addr =
                (cfg.args.base as *const usize).add(self.load_offset as usize / mem::size_of::<usize>());
            println!(
                "    Copying TEXT from {:08x}-{:08x} to {:08x}-{:08x} ({} bytes long)",
                src_addr as usize,
                src_addr as u32 + self.text_size,
                top as usize,
                top as u32 + self.text_size + 4,
                self.text_size + 4
            );
            println!(
                "    Zeroing out TEXT from {:08x}-{:08x}",
                top.add(self.text_size as usize / mem::size_of::<usize>()) as usize,
                top.add(load_size_rounded as usize / mem::size_of::<usize>()) as usize,
            );

            memcpy(top, src_addr, self.text_size as usize + 1);

            // Zero out the remaining data.
            bzero(
                top.add(self.text_size as usize / mem::size_of::<usize>()),
                top.add(load_size_rounded as usize / mem::size_of::<usize>()),
            )
        };

        // DATA SECTION
        // Round it off to a page boundary
        let load_size_rounded =
            ((self.data_size + self.bss_size) as usize + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        cfg.extra_pages += load_size_rounded / PAGE_SIZE;
        let top = cfg.get_top();
        unsafe {
            // Copy the program to the target address, rounding it off to the load size.
            let src_addr = (cfg.args.base as *const usize)
                .add((self.load_offset + self.text_size + 4) as usize / mem::size_of::<usize>() - 1);
            println!(
                "    Copying DATA from {:08x}-{:08x} to {:08x}-{:08x} ({} bytes long)",
                src_addr as usize,
                src_addr as u32 + self.data_size,
                top as usize,
                top as u32 + self.data_size,
                self.data_size
            );
            memcpy(top, src_addr, self.data_size as usize + 1);

            // Zero out the remaining data.
            println!(
                "    Zeroing out DATA from {:08x} - {:08x}",
                top.add(self.data_size as usize / mem::size_of::<usize>()) as usize,
                top.add(load_size_rounded as usize / mem::size_of::<usize>()) as usize
            );
            bzero(
                top.add(self.data_size as usize / mem::size_of::<usize>()),
                top.add(load_size_rounded as usize / mem::size_of::<usize>()),
            )
        }
    }

    /// Map this ProgramDescription into RAM.
    pub fn map(&self, cfg: &mut BootConfig) {
        let load_offset = cfg.get_top() as usize;
        println!("Mapping PID 1 into offset {:08x}", load_offset);
        let flag_defaults = FLG_R | FLG_W | FLG_VALID;
        let flag_defaults_text = FLG_R | FLG_VALID; // read-only for .text
        let stack_addr = PID1_STACK_BOTTOM;
        println!(
            "self.text_offset: {:08x}, KERNEL_LOAD_OFFSET: {:08x}",
            self.text_offset, KERNEL_LOAD_OFFSET
        );
        assert_eq!(self.text_offset as usize, KERNEL_LOAD_OFFSET);
        assert!(((self.text_offset + self.text_size) as usize) < KERNEL_MODE_STACK_BOTTOM);
        assert!(
            ((self.data_offset + self.data_size + self.bss_size) as usize) < KERNEL_MODE_STACK_BOTTOM - 16
        );
        assert!(self.data_offset as usize >= KERNEL_LOAD_OFFSET);

        // Allocate physical pages for L1 translation table
        let tt_address = cfg.alloc_l1_page_table() as usize;
        if VDBG {
            println!("Setting {:08x} as translation table address for PID 1", tt_address);
        }

        cfg.pid1.ttbr0 = tt_address;

        let translation_table = tt_address as *mut TranslationTableMemory;
        // Allocate context for this process
        let thread_address = cfg.alloc() as usize;
        if VDBG {
            println!("PID 1 thread: 0x{:08x}", thread_address);
        }

        cfg.map_page(
            translation_table,
            thread_address,
            THREAD_CONTEXT_AREA,
            FLG_R | FLG_W | FLG_VALID | FLG_DEV | FLG_NO_CACHE,
        );

        // Allocate stack pages.
        let total_stack_pages = PID1_STACK_PAGE_COUNT;

        if VDBG {
            println!("Mapping {} stack pages for PID 1", total_stack_pages);
        }

        // Allocate some continuous physical pages for stack first
        self.allocate_stack(cfg, translation_table, flag_defaults, PID1_STACK_BOTTOM, total_stack_pages);
        self.allocate_stack(
            cfg,
            translation_table,
            flag_defaults,
            KERNEL_MODE_STACK_BOTTOM,
            KERNEL_MODE_STACK_PAGE_COUNT,
        );
        self.allocate_stack(
            cfg,
            translation_table,
            flag_defaults,
            PID1_IRQ_HANDLER_STACK_BOTTOM,
            PID1_IRQ_HANDLER_STACK_PAGE_COUNT,
        );

        assert_eq!((self.text_offset as usize & (PAGE_SIZE - 1)), 0);
        assert_eq!((self.data_offset as usize & (PAGE_SIZE - 1)), 0);

        // FIXME (SFT-4004): restore the stack guard pages mapping
        println!("Mapping stack guard pages");
        //println!("PID1 stack guard: {:08x}", PID1_STACK_TOP_GUARD);
        // allocator.map_page(translation_table, 0, PID1_STACK_TOP_GUARD, FLG_GUARD | FLG_VALID);
        //println!("Kernel mode stack guard: {:08x}", KERNEL_MODE_STACK_TOP_GUARD);
        // allocator.map_page(translation_table, 0, KERNEL_MODE_STACK_TOP_GUARD, FLG_GUARD | FLG_VALID);
        println!("Finished mapping stack guard pages");

        // Map the process text section into RAM.
        // Either this is on SPI flash at an aligned address, or it
        // has been copied into RAM already.  This is why we ignore `self.load_offset`
        // and use the `load_offset` parameter instead.
        let rounded_data_bss = ((self.data_size + self.bss_size) as usize + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        // let load_size_rounded = (self.text_size as usize + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        for offset in (0..self.text_size as usize).step_by(PAGE_SIZE) {
            if VDBG {
                println!(
                    "   TEXT: Mapping {:08x} -> {:08x}",
                    load_offset + offset + rounded_data_bss,
                    self.text_offset as usize + offset
                );
            }
            cfg.map_page(
                translation_table,
                load_offset + offset + rounded_data_bss,
                self.text_offset as usize + offset,
                flag_defaults_text | FLG_X | FLG_VALID,
            );
        }

        // Map the process data section into RAM.
        for offset in (0..(self.data_size + self.bss_size) as usize).step_by(PAGE_SIZE) {
            // let page_addr = allocator.alloc();
            if VDBG {
                println!(
                    "   DATA: Mapping {:08x} -> {:08x}",
                    load_offset + offset,
                    self.data_offset as usize + offset
                );
            }
            cfg.map_page(
                translation_table,
                load_offset + offset,
                self.data_offset as usize + offset,
                flag_defaults,
            );
        }

        cfg.pid1.entrypoint = self.entrypoint as usize;
        cfg.pid1.sp = stack_addr;
    }

    fn allocate_stack(
        &self,
        allocator: &mut BootConfig,
        translation_table: *mut TranslationTableMemory,
        flags: usize,
        base: usize,
        num_pages: usize,
    ) -> usize {
        let mut stack_top_phys = 0;
        for _ in 0..num_pages {
            let phys = allocator.alloc() as usize;
            if stack_top_phys == 0 {
                stack_top_phys = phys;
            }
            println!("Allocated stack page: 0x{:08x}", phys);
        }

        for i in 0..num_pages {
            let phys = stack_top_phys - (PAGE_SIZE * num_pages) + (PAGE_SIZE * (i + 1));
            let virt = base - (PAGE_SIZE * num_pages) + (PAGE_SIZE * i);
            allocator.map_page(translation_table, phys, virt, flags);
            println!(
                "Mapped stack page ({:08x}): 0x{:08x} -> 0x{:08x}",
                translation_table as usize, virt, phys
            );
        }

        stack_top_phys
    }
}
