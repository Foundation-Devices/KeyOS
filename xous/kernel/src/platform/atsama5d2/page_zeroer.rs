// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc <hello@foundation.xyz>
// SPDX-License-Identifier: Apache-2.0

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use atsama5d27::dma::{DmaChannel, Xdmac, XdmacChannel, BIG_TRANSFER_THRESHOLD};
use keyos::{PAGE_SIZE, XDMAC1_KERNEL_ADDR};
use utralib::HW_XDMAC1_BASE;
use xous::{arch::irq::IrqNumber, MemoryFlags};

use crate::{irq::interrupt_claim_kernel, mem::MemoryManager};

// XDMAC1 channel 0 scrubs freed pages in the background; channel 1 zeroes pages for map_memory() on demand.
const BACKGROUND_CHANNEL: DmaChannel = DmaChannel::Channel0;
const MAP_MEMORY_CHANNEL: DmaChannel = DmaChannel::Channel1;

static RUNNING: AtomicBool = AtomicBool::new(false);
static INITIALIZED: AtomicBool = AtomicBool::new(false);

static CURRENT_PAGE: AtomicUsize = AtomicUsize::new(0);
static CURRENT_PAGE_NUM: AtomicUsize = AtomicUsize::new(0);

pub fn init() {
    MemoryManager::with_mut(|memory_manager| {
        memory_manager
            .map_range(
                HW_XDMAC1_BASE,
                XDMAC1_KERNEL_ADDR as *mut usize,
                0x2000,
                MemoryFlags::W | MemoryFlags::DEV,
                false,
            )
            .expect("unable to map XDMAC1 to kernel")
    });
    interrupt_claim_kernel(IrqNumber::Xdmac1, xdmac_interrupt);
    for channel in [background_channel(), map_memory_channel()] {
        channel.set_interrupt(true);
        channel.set_bi_interrupt(true);
        channel.set_di_interrupt(true);
        channel.configure_memset_transfer(atsama5d27::dma::DmaDataWidth::D32);
    }
    INITIALIZED.store(true, Ordering::SeqCst);
}

pub fn start(mm: &mut MemoryManager) {
    if RUNNING.load(Ordering::SeqCst) || !INITIALIZED.load(Ordering::SeqCst) {
        return;
    }
    let Some((phys, pages)) = mm.take_dirty_pages() else {
        return;
    };

    RUNNING.store(true, Ordering::SeqCst);
    CURRENT_PAGE.store(phys, Ordering::SeqCst);
    CURRENT_PAGE_NUM.store(pages, Ordering::SeqCst);
    background_channel().execute_transfer(0, phys as u32, pages * PAGE_SIZE / core::mem::size_of::<u32>());
}

/// Whether either XDMAC1 zeroing channel is busy: the background scrubber or the map_memory() channel.
pub fn busy() -> bool {
    RUNNING.load(core::sync::atomic::Ordering::SeqCst)
        || MemoryManager::with(|mm| mm.map_zero_ongoing().is_some())
}

/// Largest POPULATE map_memory() the on-demand zeroing channel will DMA. Above this XDMAC switches to its
/// multi-microblock path (set_data_size), which requires a BIG_TRANSFER_CHUNK_SIZE-aligned word count;
/// larger requests are rejected instead.
pub const MAX_MAP_ZERO_BYTES: usize = BIG_TRANSFER_THRESHOLD * core::mem::size_of::<u32>();

/// Start a DMA memset that zeroes the `size` bytes at physical address `phys`. `size` must be at most
/// [`MAX_MAP_ZERO_BYTES`]. The request must already be recorded with the MemoryManager; the completion
/// interrupt finishes the mapping.
pub fn start_map_zero(phys: usize, size: usize) {
    map_memory_channel().execute_transfer(0, phys as u32, size / core::mem::size_of::<u32>());
}

/// Stop the in-flight zeroing DMA. The caller must already have taken the pending job.
///
/// Used when a process dies before its zeroing finishes: the DMA must stop before the pages are reclaimed,
/// or it would keep writing into memory that has been handed to another process.
pub fn cancel_map_zero() {
    map_memory_channel().disable();
    // In case we had an interrupt while in kernel mode, make sure it doesn't fire.
    // (reading the interrupt status clears it)
    map_memory_channel().interrupt_status();
}

pub fn xdmac_interrupt() {
    let gis = Xdmac::with_alt_base_addr(XDMAC1_KERNEL_ADDR).gis();

    if gis & (1 << BACKGROUND_CHANNEL as u32) != 0 {
        // Ack the interrupt by reading it
        background_channel().interrupt_status();
        MemoryManager::with_mut(|mm| {
            mm.set_pages_to_zeroed(
                CURRENT_PAGE.swap(0, Ordering::SeqCst),
                CURRENT_PAGE_NUM.swap(0, Ordering::SeqCst),
            );
            RUNNING.store(false, Ordering::SeqCst);
            start(mm);
        });
    }

    if gis & (1 << MAP_MEMORY_CHANNEL as u32) != 0 {
        // Ack the interrupt by reading it
        map_memory_channel().interrupt_status();
        MemoryManager::with_mut(|mm| mm.map_zero_finished());
    }
}

fn background_channel() -> XdmacChannel {
    Xdmac::with_alt_base_addr(XDMAC1_KERNEL_ADDR).channel(BACKGROUND_CHANNEL)
}

fn map_memory_channel() -> XdmacChannel {
    Xdmac::with_alt_base_addr(XDMAC1_KERNEL_ADDR).channel(MAP_MEMORY_CHANNEL)
}
