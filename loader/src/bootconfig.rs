// SPDX-FileCopyrightText: 2022 Sean Cross <sean@xobs.io>
// SPDX-FileCopyrightText: 2023 Foundation Devices, Inc <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use keyos::{BOOT_SPLASH_PAGES, BOOT_SPLASH_PHYS_ADDR, PAGE_SIZE, PLAINTEXT_DRAM_END};

use crate::{args::KernelArguments, XousPid};

/// In-memory copy of the configuration page. Stage 1 sets up the gross structure,
/// and Stage 2 fills in the details.
pub struct BootConfig {
    /// Where the tagged args list starts in RAM.
    pub args: KernelArguments,

    /// Additional pages that are consumed during init.
    /// This includes pages that are allocated to other
    /// processes.
    pub extra_pages: usize,

    /// This structure keeps track of which pages are owned
    /// and which are free. A PID of `0` indicates it's free.
    pub runtime_page_tracker: &'static mut [XousPid],

    /// The size of the RPT, in bytes. Page-aligned.
    pub rpt_size_bytes: usize,

    /// Process context for PID1 and the kernel
    pub pid1: InitialProcess,
}

impl Default for BootConfig {
    fn default() -> BootConfig {
        // By setting `extra_pages`, we offset `get_top` to reserve the space used by the splash image. Make
        // sure the splash image is in fact at the top of the memory.
        const _: () = assert!(BOOT_SPLASH_PHYS_ADDR == PLAINTEXT_DRAM_END - BOOT_SPLASH_PAGES * PAGE_SIZE);
        BootConfig {
            args: KernelArguments::new(core::ptr::null::<usize>()),
            rpt_size_bytes: 0,
            extra_pages: BOOT_SPLASH_PAGES,
            runtime_page_tracker: Default::default(),
            pid1: Default::default(),
        }
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct InitialProcess {
    /// Level-1 translation table base address of the process
    pub ttbr0: usize,

    /// Where execution begins
    pub entrypoint: usize,

    /// Address of the top of the stack
    pub sp: usize,
}
