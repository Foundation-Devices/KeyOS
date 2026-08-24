// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub use atsama5d27::lcdc::ColorMode;
use atsama5d27::lcdc::{
    BurstLength, LcdDmaDesc, Lcdc, LcdcInterruptStatus, LcdcLayerId, LcdcLayerInterruptStatus,
};
use embedded_hal::spi::SpiDevice;
use gui_server_api::consts::{SCREEN_HEIGHT, SCREEN_WIDTH};
use server::MessageId as _;
use utralib::HW_LCDC_BASE;
use xous::{arch::irq::IrqNumber, MemoryRange, ScalarMessage, CID};

use super::DEFAULT_BACKLIGHT_LEVEL_PERCENT;
use crate::{
    handlers::OnVsyncMessage,
    layers::{Layer, LayerPixelFormat, LayerStack},
    Gui, PowerManagerApi,
};

spi::use_api!();

pub const MAX_LAYERS: usize = 4;

/// The LCDC register map ends with HEOCLUT255 at offset 0x15fc, so it occupies two pages.
const LCDC_MMIO_SIZE: usize = xous::PAGE_SIZE * 2;

/// Vertical front porch, in lines, applied at startup. Sized so the vertical
/// blanking interval (~1.1 ms) outlasts typical in-kernel stalls observed at
/// time of writing (~700 us) plus the commit handler, leaving the in-interrupt
/// layer commit room to land before the next start-of-frame.
const VERTICAL_FRONT_PORCH: u16 = 40;

static VSYNC_HAPPENED: AtomicBool = AtomicBool::new(false);

/// Hands the most recent composited layer stack from the main thread to the
/// LCDC interrupt handler, which commits it during the vertical blanking
/// interval. The handler must never block, so this cannot be a lock.
///
/// The main thread is the only writer and the interrupt is the only reader.
/// The writer always writes the slot that is not currently published and only
/// then publishes it, so the reader (which only ever copies the published
/// slot) can never observe a slot mid-write -- even though the interrupt can
/// preempt the writer at any instruction.
struct LayerStaging {
    slots: [UnsafeCell<LayerStack>; 2],
    front: AtomicUsize,
}

// SAFETY: the UnsafeCell slots make this !Sync by default. Sharing it between
// the main thread and the interrupt is sound because they never touch the same
// slot at once: the writer only mutates the unpublished slot before publishing
// it, and the reader only copies the published slot. The Release/Acquire on
// `front` keeps a slot's contents ordered before its publication, so no slot is
// ever read while being written.
unsafe impl Sync for LayerStaging {}

impl LayerStaging {
    fn new() -> Self {
        Self {
            slots: [UnsafeCell::new(LayerStack::default()), UnsafeCell::new(LayerStack::default())],
            front: AtomicUsize::new(0),
        }
    }

    fn stage(&self, layers: LayerStack) {
        let back = 1 - self.front.load(Ordering::Relaxed);
        unsafe { *self.slots[back].get() = layers };
        self.front.store(back, Ordering::Release);
    }

    fn latest(&self) -> LayerStack {
        let front = self.front.load(Ordering::Acquire);
        unsafe { *self.slots[front].get() }
    }
}

pub struct PlatformDisplay {
    lcdc_addr: MemoryRange,
    lcdc: Lcdc,
    spi: SpiPeripheral,
    power_manager: PowerManagerApi,
    dma_descriptors: MemoryRange,
    // Leaked so it lives forever, since the interrupt handler also holds it.
    staging: &'static LayerStaging,
    curr_backlight_level: u8, // 0x00(max)..0xff(min)
    lcd_on: bool,
    dimmed: bool,
}

struct InterruptContext {
    lcdc: Lcdc,
    dma_descriptors: MemoryRange,
    staging: &'static LayerStaging,
    cid: CID,
    update_pending_warn_countdown: usize,
    fifo_underflow_warn_countdown: usize,
}

impl PlatformDisplay {
    pub(crate) fn init(initial_base: Layer) -> Self {
        let power_manager = PowerManagerApi::default();
        power_manager.enable_peripheral(atsama5d27::pmc::PeripheralId::Lcdc).expect("Could not enabled LCD");
        let lcdc_addr = xous::syscall::map_memory(
            xous::MemoryAddress::new(HW_LCDC_BASE),
            None,
            LCDC_MMIO_SIZE,
            xous::MemoryFlags::W | xous::MemoryFlags::DEV,
        )
        .expect("Could not map LCDC");
        let mut lcdc = Lcdc::new_vma(lcdc_addr.as_mut_ptr() as _, SCREEN_WIDTH as u16, SCREEN_HEIGHT as u16);
        lcdc.wait_for_sync_in_progress();
        lcdc.set_vertical_front_porch_width(VERTICAL_FRONT_PORCH);
        let spi =
            SpiApi::default().claim_peripheral(spi::Peripheral::Lcd).expect("Could not claim SPI peripheral");

        let mut dma_descriptors = xous::map_memory(
            None,
            None,
            0x1000,
            xous::MemoryFlags::W
                | xous::MemoryFlags::NO_CACHE
                | xous::MemoryFlags::DEV
                | xous::MemoryFlags::POPULATE
                | xous::MemoryFlags::PLAINTEXT,
        )
        .expect("Could not map uncached memory for DMA");
        let staging: &'static LayerStaging = Box::leak(Box::new(LayerStaging::new()));
        for layer in [LcdcLayerId::Base, LcdcLayerId::Heo, LcdcLayerId::Ovr1, LcdcLayerId::Ovr2] {
            lcdc.set_transfer_descriptor_fetch_enable(layer, true);
            lcdc.set_blender_overlay_layer_enable(layer, true);
            lcdc.set_blender_dma_layer_enable(layer, true);

            lcdc.set_blender_global_alpha_enable(layer, true);
            lcdc.set_blender_chroma_key_enable(layer, false);

            lcdc.set_use_dma_path_enable(layer, true);

            lcdc.set_system_bus_dma_burst_length(layer, BurstLength::Incr16);
            lcdc.set_system_bus_dma_burst_enable(layer, true);

            lcdc.set_blender_use_iterated_color(layer, true);
            lcdc.set_blender_iterated_color_enable(layer, true);

            let dma = dma_desc_for_layer(&mut dma_descriptors, layer);
            let dma_phys = xous::virt_to_phys(dma as *mut _ as usize).expect("DMA physical address") as u32;
            dma.addr = 0;
            dma.ctrl = 1;
            dma.next = dma_phys;
            lcdc.set_dma_head_pointer(layer, dma_phys);
            lcdc.set_add_to_queue_enable(layer, true);
        }
        // Make sure both master interfaces are used on the LCDC, and that
        // Base and Heo are on different interfaces
        lcdc.set_sif(LcdcLayerId::Base, true);
        lcdc.set_sif(LcdcLayerId::Ovr2, true);

        let mut layers = LayerStack::default();
        layers.push(initial_base);
        // The interrupt handler is not claimed yet: stage so its first read has
        // a frame, and commit so one is on screen before it takes over.
        staging.stage(layers);
        commit_layers(&lcdc, &mut dma_descriptors, layers);

        Self {
            lcdc_addr,
            lcdc,
            spi,
            power_manager,
            dma_descriptors,
            staging,
            curr_backlight_level: Self::backlight_level_pct_to_pwm(DEFAULT_BACKLIGHT_LEVEL_PERCENT),
            lcd_on: true,
            dimmed: false,
        }
    }

    pub(crate) fn subscribe_to_vsync(&self, context: &mut server::ServerContext<Gui>) {
        let interrupt_context = Box::into_raw(Box::new(InterruptContext {
            lcdc: Lcdc::new_vma(self.lcdc_addr.as_mut_ptr() as _, SCREEN_WIDTH as u16, SCREEN_HEIGHT as u16),
            dma_descriptors: self.dma_descriptors,
            staging: self.staging,
            cid: xous::connect(context.sid()).expect("Could not connect to self"),
            update_pending_warn_countdown: 0,
            fifo_underflow_warn_countdown: 0,
        }));

        xous::claim_interrupt(IrqNumber::Lcdc, lcdc_irq_handler, interrupt_context as _)
            .expect("Could not claim LCDC interrupt");
        self.lcdc.enable_dma_transfer_done_interrupt(LcdcLayerId::Base, true);
        self.lcdc.enable_layer_interrupts(LcdcLayerId::Base, true);
    }

    pub(crate) fn setup_layers(&mut self, layers: LayerStack) { self.staging.stage(layers); }

    pub(crate) fn is_lcd_on(&self) -> bool { self.lcd_on }

    pub(crate) fn is_dimmed(&self) -> bool { self.dimmed }

    pub(crate) fn turn_lcd_off(&mut self) {
        log::debug!("Turning LCD off");
        self.lcdc.disable_display();

        // Put the LCD controller itself into low-power mode
        if let Err(e) = self.spi.write(&[0x10u16]) {
            log::error!("Error sending \"Sleep In\" on SPI: {e:?}");
        }

        if let Err(e) = self.power_manager.disable_peripheral(atsama5d27::pmc::PeripheralId::Lcdc) {
            log::error!("Error disabling clock to Lcdc: {e:?}");
        }
        self.lcd_on = false;
    }

    pub(crate) fn turn_lcd_on(&mut self) {
        log::debug!("Turning LCD on");

        if let Err(e) = self.power_manager.enable_peripheral(atsama5d27::pmc::PeripheralId::Lcdc) {
            log::error!("Error enabling clock to Lcdc: {e:?}");
            return;
        }

        // Wake up the LCD itself
        if let Err(e) = self.spi.write(&[0x11u16]) {
            log::error!("Error sending \"Sleep Out\" on SPI: {e:?}");
        }

        self.lcdc.enable_display();

        self.lcd_on = true;
        self.dimmed = false;
    }

    #[inline(always)]
    const fn backlight_level_pct_to_pwm(percent: u8) -> u8 {
        0xff_u8.saturating_sub((percent as u32 * 0xFF / 100) as u8)
    }

    pub(crate) fn set_backlight_level_pct(&mut self, percent: u8) {
        if !self.lcd_on {
            log::warn!("Called while lcd was off");
            return;
        }
        self.curr_backlight_level = Self::backlight_level_pct_to_pwm(percent.clamp(0, 100));
        self.lcdc.wait_for_sync_in_progress();
        self.lcdc.set_pwm_compare_value(self.curr_backlight_level);
        self.dimmed = false;
    }

    #[cfg(not(feature = "recovery-os"))]
    pub(crate) fn dim(&mut self) { self.dimmed = true; }

    pub fn vsync_happened() -> bool { VSYNC_HAPPENED.swap(false, std::sync::atomic::Ordering::Relaxed) }
}

fn dma_desc_for_layer(dma_descriptors: &mut MemoryRange, layer: LcdcLayerId) -> &mut LcdDmaDesc {
    let descs = dma_descriptors.as_slice_mut::<LcdDmaDesc>();
    match layer {
        LcdcLayerId::Base => &mut descs[0],
        LcdcLayerId::Heo => &mut descs[1],
        LcdcLayerId::Ovr1 => &mut descs[2],
        LcdcLayerId::Ovr2 => &mut descs[3],
    }
}

/// Writes the layer stack to the LCDC registers and descriptors. Runs in the
/// interrupt handler during the vertical blanking interval, so the self-linked
/// descriptor is mutated in place while the controller is not reading it, and
/// the attribute latch lands on the same start of frame as the new frame
/// buffer address.
fn commit_layers(lcdc: &Lcdc, dma_descriptors: &mut MemoryRange, mut layers: LayerStack) {
    // Only HEO is actually capable of scaling, so reorder layers so that
    // overlay[1] is always HEO and overlay[2] is always OVR1, and set HEO priority instead.
    // LayerStack guarantees that we have only one scaling layer, and it's not the last.
    if layers.layers[2].as_ref().map(|l| l.is_scaled()).unwrap_or(false) {
        layers.layers.swap(1, 2);
        lcdc.set_heo_on_top(true);
    } else {
        lcdc.set_heo_on_top(false);
    }

    for (layer_conf, layer) in
        layers.layers.iter().zip([LcdcLayerId::Base, LcdcLayerId::Heo, LcdcLayerId::Ovr1, LcdcLayerId::Ovr2])
    {
        let Some(layer_conf) = layer_conf else {
            lcdc.set_use_dma_path_enable(layer, false);
            lcdc.set_channel_enable(layer, false);
            continue;
        };
        let (x, y) = layer_conf.dst_pos();
        let (dst_w, dst_h) = layer_conf.dst_dimensions();

        lcdc.set_window_size(layer, dst_w as u16, dst_h as u16);
        lcdc.set_window_pos(layer, x as u16, y as u16);

        let (crop_w, crop_h) = if layer == LcdcLayerId::Base {
            // Base layer disregards cropping, and this will be important
            // in the stride calculation
            (SCREEN_WIDTH, SCREEN_HEIGHT)
        } else {
            layer_conf.crop_dimensions()
        };
        if layer == LcdcLayerId::Heo {
            lcdc.set_heo_mem_size(crop_w as u16, crop_h as u16);
            lcdc.set_heo_scaling(layer_conf.is_scaled());
        }

        // Limitation: if we use local alpha, the LCDC will not apply the global alpha.
        // Limitation: HEO does not seem to compute local alpha when scaling
        let mut local_alpha = layer_conf.alpha() == 255 && !layer_conf.is_scaled();

        let rgb_mode = match layer_conf.pixel_format() {
            LayerPixelFormat::Argb8888 => ColorMode::Argb8888,
            LayerPixelFormat::Rgb565 => {
                local_alpha = false;
                ColorMode::Rgb565
            }
        };
        lcdc.set_rgb_mode_input(layer, rgb_mode);

        match layer_conf.src() {
            crate::layers::SourceType::Dma { phys: mut src, range } => {
                lcdc.set_use_dma_path_enable(layer, true);
                let (src_w, src_h) = layer_conf.src_dimensions();
                let bpp = layer_conf.pixel_format().bytes_per_pixel();
                let Some(src_len) = src_w.checked_mul(src_h).and_then(|px| px.checked_mul(bpp)) else {
                    log::error!("Skipping layer with overflowing dimensions: {layer_conf:?}");
                    lcdc.set_channel_enable(layer, false);
                    continue;
                };
                if range.len() < src_len {
                    log::error!("Skipping layer with invalid framebuffer span: {layer_conf:?}");
                    lcdc.set_channel_enable(layer, false);
                    continue;
                }
                let (crop_x, crop_y) = layer_conf.crop_pos();

                src += (crop_x + crop_y * src_w) * bpp;
                if layer == LcdcLayerId::Base {
                    // XXX: We try to emulate at least horizontal position here, but it will
                    // only work in very special cases, and only if an overlay
                    // overwrites the junk pixels we will inevitably render.
                    src -= x * bpp;
                }
                let stride = (src_w - crop_w) * bpp;
                dma_desc_for_layer(dma_descriptors, layer).addr = src as u32;
                lcdc.set_horiz_stride(layer, stride as i32);
            }
            crate::layers::SourceType::Color { r, g, b } => {
                if layer == LcdcLayerId::Base {
                    // Disabling Base DMA would stop the only source of the vsync
                    // interrupt that drives this pump and stall it, so leave it
                    // running and keep the previous base on screen instead. The
                    // state machine keeps the base framebuffer-backed
                    // (switch_to_window_with_nav gates on most_recent_buffer),
                    // so reaching here means that invariant has broken.
                    log::warn!("LCDC base layer has no framebuffer; keeping the previous base");
                } else {
                    lcdc.set_use_dma_path_enable(layer, false);
                }
                lcdc.set_default_color(layer, r, g, b);
                local_alpha = false;
            }
        };

        lcdc.set_blender_local_alpha_enable(layer, local_alpha);
        lcdc.blender_set_global_alpha(layer, layer_conf.alpha());

        lcdc.set_channel_enable(layer, true);
    }

    // Arm every layer's attribute latch in one write so a start-of-frame
    // cannot consume only part of the commit.
    lcdc.update_all_attributes();
}

/// The interrupt fires every frame, so a persistent fault would flood the log;
/// only one warning in this many is emitted.
const WARN_THROTTLE: usize = 40;

fn lcdc_irq_handler(_irq_no: usize, arg: *mut usize) {
    let ctx = unsafe { &mut *(arg as *mut InterruptContext) };
    // This read clears the flag and acknowledges the interrupt.
    if !ctx.lcdc.layer_interrupt_status(LcdcLayerId::Base).contains(LcdcLayerInterruptStatus::DMA) {
        return;
    }

    // We are in the vertical blanking interval now. The previous commit's
    // attribute latch should already have been consumed at the start of this
    // frame; if it has not, we did not finish committing inside the window.
    // commit_layers arms all layers in one write (update_all_attributes), so
    // the latch is all-or-nothing and Base alone reflects the whole commit.
    let overrun = ctx.lcdc.is_update_pending(LcdcLayerId::Base);
    if overrun {
        if ctx.update_pending_warn_countdown == 0 {
            log::warn!("LCDC attribute update still pending at DMA interrupt; missed the vsync window");
            ctx.update_pending_warn_countdown = WARN_THROTTLE;
        }
        ctx.update_pending_warn_countdown -= 1;
    }
    if ctx.lcdc.interrupt_status().contains(LcdcInterruptStatus::FIFOERR) {
        if ctx.fifo_underflow_warn_countdown == 0 {
            log::warn!("LCDC output FIFO underflow");
            ctx.fifo_underflow_warn_countdown = WARN_THROTTLE;
        }
        ctx.fifo_underflow_warn_countdown -= 1;
    }

    // Re-arm the latest layers regardless: a late commit still lands at the
    // next frame boundary.
    let layers = ctx.staging.latest();
    commit_layers(&ctx.lcdc, &mut ctx.dma_descriptors, layers);

    // On an overrun the buffer swap is deferred one frame, so do not advance
    // the consumer's bookkeeping yet: that would let it reclaim a buffer the
    // controller may still be scanning out. Let the next clean vsync drive it.
    if overrun {
        return;
    }

    VSYNC_HAPPENED.store(true, Ordering::Relaxed);
    if let Err(e) = xous::try_send_message(
        ctx.cid,
        xous::Message::Scalar(ScalarMessage { id: OnVsyncMessage::ID, ..Default::default() }),
    ) {
        log::error!("Could not send OnVSyncMessage: {e:?}");
    }
}
