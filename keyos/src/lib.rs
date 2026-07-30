// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#![no_std]

pub mod batt;
pub mod recovery;

#[cfg(keyos)]
pub mod stack_canary;

pub const PAGE_SIZE: usize = 4096;

pub const TOTAL_FLASH_BLOCKS: usize = 0x74a6000; // As per EMMC64G-TY29-5B101 datasheet Table 4

// MCK: 164MHz, which is divided by 2 because of the default `h32mxdiv` PMC setting
pub const MASTER_CLOCK_SPEED: u32 = 164000000 / 2;

// ------------------------ User-accessible addresses ------------------------

pub const ASLR_START: usize = 0x0001_0000;
pub const ASLR_END: usize = 0x3000_0000;

// Virtual address for normal memory allocations
pub const MMAP_AREA_VIRT: usize = 0x3000_0000;
pub const MMAP_AREA_VIRT_END: usize = 0x4000_0000;

pub const MEMORY_MIRROR_AREA_VIRT: usize = 0x4000_0000;

pub const RAW_ELF_TEMPORARY_ADDRESS: usize = 0x5000_0000; // Only used during process start
pub const BOOT_SPLASH_FB: usize = 0x5000_0000; // Mapped to PID1 by the loader, and then to gui-server by the kernel

// The whole kernel argument table, including all embedded init processes.
// Only mapped to PID1, which unmaps the vast majority of it.
pub const KERNEL_ARGUMENT_OFFSET: usize = 0x5800_0000;

pub const USER_IRQ_STACK_BOTTOM: usize = 0x6fe0_0000;
pub const USER_IRQ_STACK_PAGE_COUNT: usize = 3;

pub const USER_STACK_BOTTOM: usize = 0x6ff0_0000;
pub const STACK_PAGE_COUNT: usize = 64;
pub const USER_STACK_TOP_GUARD: usize = USER_STACK_BOTTOM - PAGE_SIZE * (STACK_PAGE_COUNT + 1);

pub const USER_AREA_END: usize = 0x7000_0000;

// ------------------------ Per-process kernel-accessible addresses ------------------------

pub const THREAD_CONTEXT_AREA: usize = 0x7000_0000;
pub const L1_USER_PAGE_TABLE_PAGES: usize = 2;
pub const L1_USER_PAGE_TABLE_ENTRIES: usize = 2048;

// ------------------------ Global kernel-accessible addresses ------------------------
pub const TTBR1_SPLIT: usize = 0x8000_0000;

pub const KSET_MEMORY_BASE: usize = 0x9000_0000;

pub const MAPPED_PHYSICAL_RAM: usize = 0xA000_0000;

/// RTT control block virtual address.
/// Specify this address in the JLinkRTTViewer or SystemView as an RTT control block address.
#[cfg(feature = "trace-systemview")]
pub const RTT_CONTROL_BLOCK_VIRT_ADDR: usize = 0xbeef_0000;

/// Start address of the area where the RTT buffers will be allocated.
#[cfg(feature = "trace-systemview")]
pub const RTT_BUFFERS_START_VIRT_ADDR: usize = 0xbeef_1000;

pub const ALLOCATION_TRACKER_OFFSET: usize = 0xff80_8000;
pub const ALLOCATION_TRACKER_PAGES_MAX: usize = 32;

// Virtual addresses of kernel-mapped peripherals
#[cfg(feature = "trace-systemview")]
pub const PIT_KERNEL_ADDR: usize = 0xffc1_0000;
pub const PMC_KERNEL_ADDR: usize = 0xffc2_0000;
pub const DDRC_KERNEL_ADDR: usize = 0xffc3_0000;
pub const RSTC_KERNEL_ADDR: usize = 0xffc4_0000;
pub const SECURAM_KERNEL_ADDR: usize = 0xffc5_0000;
pub const XDMAC1_KERNEL_ADDR: usize = 0xffc6_0000;
pub const TC0_KERNEL_ADDR: usize = 0xffc7_0000;
pub const L2CC_KERNEL_ADDR: usize = 0xffc8_0000;
pub const SFR_KERNEL_ADDR: usize = 0xffc9_0000;
pub const AIC_KERNEL_ADDR: usize = 0xffca_0000;
pub const SAIC_KERNEL_ADDR: usize = 0xffcb_0000; // Secure version of AIC
pub const RXLP_KERNEL_ADDR: usize = 0xffcc_0000;
pub const SFC_KERNEL_ADDR: usize = 0xffcd_0000;
pub const TRNG_KERNEL_ADDR: usize = 0xffce_0000;

pub const ICM_KERNEL_ADDR: usize = 0xffce_4000;
pub const ICM_KERNEL_DESC_AREA_ADDR: usize = 0xffce_8000; // used by ICM
pub const ICM_KERNEL_HASH_AREA_ADDR: usize = 0xffce_9000; // used by ICM

pub const UART_ADDR: usize = 0xffcf_0000;

pub const KERNEL_LOAD_OFFSET: usize = 0xffd0_0000;
pub const NUM_KERNEL_PAGES_MAX: usize = 128;

pub const PID1_STACK_BOTTOM: usize = 0xfff8_0000;
pub const PID1_STACK_PAGE_COUNT: usize = 16;
pub const PID1_STACK_TOP_GUARD: usize = PID1_STACK_BOTTOM - PAGE_SIZE * (PID1_STACK_PAGE_COUNT + 1);

pub const PID1_IRQ_HANDLER_STACK_BOTTOM: usize = 0xfffe_8000;
pub const PID1_IRQ_HANDLER_STACK_PAGE_COUNT: usize = 4;
pub const PID1_IRQ_HANDLER_STACK_TOP_GUARD: usize =
    PID1_IRQ_HANDLER_STACK_BOTTOM - PAGE_SIZE * (PID1_IRQ_HANDLER_STACK_PAGE_COUNT + 1);

pub const KERNEL_MODE_STACK_BOTTOM: usize = 0xffff_0000;
pub const KERNEL_MODE_STACK_PAGE_COUNT: usize = 8;
pub const KERNEL_MODE_STACK_TOP_GUARD: usize =
    KERNEL_MODE_STACK_BOTTOM - PAGE_SIZE * (KERNEL_MODE_STACK_PAGE_COUNT + 1);

// ------------------------ Physical addresses ------------------------

pub const IDLE_FUNCTION_PHYS_ADDR: usize = 0x0021f000; // Last page of SRAM0
pub const IDLE_FUNCTION_MEM_SIZE: usize = 0x1000;

pub const RAM_SIZE: usize = 128 * 1024 * 1024;

pub const RAM_PAGES: usize = RAM_SIZE / PAGE_SIZE;

/// Regular (plaintext) DRAM base physical address.
pub const PLAINTEXT_DRAM_BASE: usize = 0x20000000;

/// Actual load address is RAM start, but there is a 0x800 cosign2 header
pub const LOADER_CODE_ADDRESS: usize = 0x20000800;

/// Enough pages for the framebuffer and the DMA desc
pub const BOOT_SPLASH_PAGES: usize = 0x178;

pub const BOOT_SPLASH_PHYS_ADDR: usize = PLAINTEXT_DRAM_END - BOOT_SPLASH_PAGES * PAGE_SIZE;

/// End of the regular (plaintext) DRAM section.
pub const PLAINTEXT_DRAM_END: usize = PLAINTEXT_DRAM_BASE + RAM_SIZE;

/// Physical address of the AES Bridge (`AESB`) for encrypted DRAM.
pub const ENCRYPTED_DRAM_BASE: usize = 0x40000000;

/// End of the `AESB` memory section.
pub const ENCRYPTED_DRAM_END: usize = ENCRYPTED_DRAM_BASE + RAM_SIZE;

/// Marker for reserved encrypted pages.
pub const RESERVED_ENCRYPTED_PHYS_ADDR_MARKER: usize = ENCRYPTED_DRAM_BASE;

/// Converts a regular DRAM physical address into `AESB` with the same offset.
pub fn to_encrypted_phys_addr(addr: usize) -> usize { ENCRYPTED_DRAM_BASE | addr & (PLAINTEXT_DRAM_BASE - 1) }

/// Converts `AESB` physical address into regular `DRAM` address.
pub fn to_plaintext_phys_addr(addr: usize) -> usize { PLAINTEXT_DRAM_BASE | addr & (ENCRYPTED_DRAM_BASE - 1) }

/// Checks if the physical address is within the `AESB` area.
pub fn is_address_encrypted(addr: usize) -> bool { (ENCRYPTED_DRAM_BASE..ENCRYPTED_DRAM_END).contains(&addr) }

/// Checks if the physical address is within the plaintext `DRAM` area.
pub fn is_address_in_plaintext_dram(addr: usize) -> bool {
    (PLAINTEXT_DRAM_BASE..PLAINTEXT_DRAM_END).contains(&addr)
}
