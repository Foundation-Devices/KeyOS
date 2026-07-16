//! LCD controller (LCDC) implementation.

use utralib::{utra::lcdc::*, HW_LCDC_BASE, *};

#[repr(C, align(8))]
#[derive(Debug, Default)]
pub struct LcdDmaDesc {
    pub addr: u32,
    pub ctrl: u32,
    pub next: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[allow(dead_code)]
#[repr(u8)]
pub enum LcdcLayerId {
    /// Base layer.
    Base = 0,

    /// Overlay layer 1.
    Ovr1 = 1,

    /// Overlay layer 2.
    Ovr2 = 2,

    /// High-end overlay.
    Heo = 3,
}

#[derive(Debug)]
#[repr(u8)]
#[allow(dead_code)]
enum LcdcPwmClockSource {
    Slow = 0,
    System,
}

#[derive(Debug)]
#[repr(u8)]
#[allow(dead_code)]
enum LcdcClockSource {
    System = 0,
    System2x,
}

#[derive(Debug)]
#[allow(dead_code)]
enum OutputColorMode {
    Mode12bpp = 0,
    Mode16Bpp = 1,
    Mode18Bpp = 2,
    Mode24Bpp = 3,
}

#[derive(Debug)]
#[allow(dead_code)]
enum VsyncSyncEdge {
    First = 0,
    Second,
}

#[derive(Debug)]
#[allow(dead_code)]
enum SignalPolarity {
    Positive = 0,
    Negative,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum ColorMode {
    Rgb444 = 0,
    Argb444,
    Rgba444,
    Rgb565,
    Trgb1555,
    Rgb666,
    Rgb666Packed,
    Trgb1666,
    TrgbPacked,
    Rgb888,
    Rgb888Packed,
    Trgb1888,
    Argb8888,
    Rgba8888,
    Lut8,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum BurstLength {
    Single = 0,
    Incr4,
    Incr8,
    Incr16,
}

const PWM_CLOCK_SOURCE: LcdcPwmClockSource = LcdcPwmClockSource::System;
const PWM_PRESCALER: u8 = 5;
const HSYNC_LENGTH: u16 = 60;
const VSYNC_LENGTH: u16 = 60;
const PIXEL_CLOCK_DIV: u8 = 16;
const DISPLAY_GUARD_NUM_FRAMES: u16 = 1;
const OUTPUT_COLOR_MODE: OutputColorMode = OutputColorMode::Mode24Bpp;
const SYNC_EDGE: VsyncSyncEdge = VsyncSyncEdge::First;
const VSYNC_POLARITY: SignalPolarity = SignalPolarity::Negative;
const HSYNC_POLARITY: SignalPolarity = SignalPolarity::Negative;
const DEFAULT_BRIGHTNESS_PCT: u32 = 55;
const PWM_SIGNAL_POLARITY: SignalPolarity = SignalPolarity::Positive;
pub const DEFAULT_GFX_COLOR_MODE: ColorMode = ColorMode::Argb8888;
const VFP: u16 = 15;
const VBP: u16 = 31;
const HFP: u16 = 8;
const HBP: u16 = 12;

pub struct LayerConfig {
    id: LcdcLayerId,
    fb_phys_addr: usize,
    dma_desc_addr: usize,
    dma_desc_phys_addr: usize,
}

impl LayerConfig {
    #[inline]
    pub fn new(
        id: LcdcLayerId,
        fb_phys_addr: usize,
        dma_desc_addr: usize,
        dma_desc_phys_addr: usize,
    ) -> LayerConfig {
        Self {
            id,
            fb_phys_addr,
            dma_desc_addr,
            dma_desc_phys_addr,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug, Copy, Clone)]
    pub struct LcdcInterruptStatus: u32 {
        const SOF     = 1 << 0;
        const DIS     = 1 << 1;
        const DISP    = 1 << 2;
        const FIFOERR = 1 << 4;
        const BASE    = 1 << 8;
        const OVR1    = 1 << 9;
        const OVR2    = 1 << 10;
        const HEO     = 1 << 11;
        const PP      = 1 << 13;
    }
}

bitflags::bitflags! {
    #[derive(Debug, Copy, Clone)]
    pub struct LcdcLayerInterruptStatus: u32 {
        const DMA  = 1 << 2;
        const DSCR = 1 << 3;
        const ADD  = 1 << 4;
        const DONE = 1 << 5;
        const OVR  = 1 << 6;
    }
}

pub struct Lcdc {
    base_addr: u32,
    w: u16,
    h: u16,
}

impl Lcdc {
    #[inline]
    pub fn new(w: u16, h: u16) -> Lcdc {
        Lcdc {
            base_addr: HW_LCDC_BASE as u32,
            w,
            h,
        }
    }

    /// Creates a new LCDC instance with the specified base address and
    /// DMA descriptor's virtual and physical addresses.
    #[inline]
    pub fn new_vma(base_addr: u32, w: u16, h: u16) -> Lcdc {
        Lcdc { base_addr, w, h }
    }

    #[inline]
    pub fn init(&mut self, layers: &[LayerConfig], cache_maintenance: impl Fn()) {
        // Configure the LCD timing parameters
        self.wait_for_sync_in_progress();
        self.select_pwm_clock_source(PWM_CLOCK_SOURCE);
        self.wait_for_sync_in_progress();
        self.set_clock_divider(PIXEL_CLOCK_DIV);

        self.wait_for_sync_in_progress();
        self.set_hsync_pulse_width(HSYNC_LENGTH);
        self.wait_for_sync_in_progress();
        self.set_vsync_pulse_width(VSYNC_LENGTH);

        self.wait_for_sync_in_progress();
        self.set_vertical_front_porch_width(VFP); //Set the vertical porches
        self.wait_for_sync_in_progress();
        self.set_vertical_back_porch_width(VBP);

        self.wait_for_sync_in_progress();
        self.set_horizontal_front_porch_width(HFP); //Set the horizontal porches
        self.wait_for_sync_in_progress();
        self.set_horizontal_back_porch_width(HBP);

        self.wait_for_sync_in_progress();
        self.set_num_active_rows(self.h);
        self.wait_for_sync_in_progress();
        self.set_num_pixels_per_line(self.w);

        self.wait_for_sync_in_progress();
        self.set_lcdc_clk_polarity(true);

        self.wait_for_sync_in_progress();
        self.set_lcdc_clk_source(true);

        self.wait_for_sync_in_progress();
        self.set_display_guard_time(DISPLAY_GUARD_NUM_FRAMES);
        self.wait_for_sync_in_progress();
        self.set_output_mode(OUTPUT_COLOR_MODE);
        self.wait_for_sync_in_progress();
        self.set_display_signal_synchronization(true);
        self.wait_for_sync_in_progress();
        self.set_vsync_pulse_start(SYNC_EDGE);
        self.wait_for_sync_in_progress();
        self.set_vsync_polarity(VSYNC_POLARITY);
        self.wait_for_sync_in_progress();
        self.set_hsync_polarity(HSYNC_POLARITY);

        self.wait_for_sync_in_progress();
        self.set_pwm_signal_polarity(PWM_SIGNAL_POLARITY);
        self.wait_for_sync_in_progress();
        self.set_pwm_prescaler(PWM_PRESCALER);

        self.wait_for_sync_in_progress();
        self.set_pwm_compare_value(
            0xff_u8.saturating_sub((DEFAULT_BRIGHTNESS_PCT * 0xFF / 100) as u8),
        );

        for layer in layers {
            self.update_layer(layer, &cache_maintenance);
            self.enable_layer(layer.id);
        }

        self.enable_display();
    }

    #[inline]
    pub fn update_layer(&self, layer: &LayerConfig, cache_maintenance: impl Fn()) {
        let dma_desc = layer.dma_desc_addr as *mut LcdDmaDesc;
        unsafe {
            (*dma_desc).addr = layer.fb_phys_addr as u32;
            (*dma_desc).ctrl = 0x01;
            (*dma_desc).next = layer.dma_desc_phys_addr as u32;
        }

        cache_maintenance();

        self.set_dma_head_pointer(layer.id, layer.dma_desc_phys_addr as u32);
        self.add_dma_desc_to_queue(layer.id);
    }

    #[inline]
    pub fn enable_layer(&self, layer: LcdcLayerId) {
        self.set_transfer_descriptor_fetch_enable(layer, true);
        self.set_blender_overlay_layer_enable(layer, true);
        self.set_blender_dma_layer_enable(layer, true);

        self.set_blender_global_alpha_enable(layer, true);
        self.set_blender_chroma_key_enable(layer, false);
        self.blender_set_global_alpha(layer, 0xff);
        self.set_blender_local_alpha_enable(layer, false);

        self.set_use_dma_path_enable(layer, true);
        self.set_rgb_mode_input(layer, DEFAULT_GFX_COLOR_MODE);

        self.set_transfer_descriptor_fetch_enable(layer, true);
        self.update_overlay_attributes_enable(layer);
        self.update_attribute(layer);

        self.set_system_bus_dma_burst_length(layer, BurstLength::Incr16);
        self.set_system_bus_dma_burst_enable(layer, true);

        self.set_channel_enable(layer, true);
    }

    #[inline]
    pub fn enable_display(&mut self) {
        // 1. Enable pixel clock
        self.wait_for_sync_in_progress();
        self.set_pixel_clock_enable(true);

        // 2. Check that the clock is running
        self.wait_for_clock_running();

        // 3. Enable Horizontal and Vertical Synchronization
        self.wait_for_sync_in_progress();
        self.set_sync_enable(true);

        // 4. Check that synchronization is up
        self.wait_for_sync();

        // 5. Enable the display power signal
        self.wait_for_sync_in_progress();
        self.set_disp_signal_enable(true);

        // 6. Wait for power signal to be activated
        self.wait_for_disp_signal();

        // 7. Enable the backlight
        self.wait_for_sync_in_progress();
        self.set_pwm_enable(true);
    }

    #[inline]
    pub fn disable_display(&mut self) {
        // 1. Disable the backlight
        self.wait_for_sync_in_progress();
        self.set_pwm_enable(false);

        // 2. Enable the display power signal
        self.wait_for_sync_in_progress();
        self.set_disp_signal_enable(false);

        // 3. Wait for power signal to be deactivated
        self.wait_for_disp_off();

        // 4. Disable Horizontal and Vertical Synchronization
        self.wait_for_sync_in_progress();
        self.set_sync_enable(false);

        // 5. Check that synchronization is down
        self.wait_for_sync_off();

        // 6. Enable pixel clock
        self.wait_for_sync_in_progress();
        self.set_pixel_clock_enable(false);

        // 7. Check that the clock is running
        self.wait_for_clock_stopped();
    }

    #[inline]
    pub fn disable_layer(&self, layer: LcdcLayerId) {
        self.set_use_dma_path_enable(layer, false);
        self.set_transfer_descriptor_fetch_enable(layer, false);
        self.update_overlay_attributes_enable(layer);
        self.update_attribute(layer);
        self.set_system_bus_dma_burst_enable(layer, false);
        self.set_channel_enable(layer, false);
    }

    #[inline]
    pub fn set_lcdc_clk_source(&mut self, is_x2: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG0_CLKSEL, is_x2 as u32)
    }

    fn set_lcdc_clk_polarity(&mut self, on_falling: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG0_CLKPOL, on_falling as u32)
    }

    #[inline]
    pub fn wait_for_sync_in_progress(&self) {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        while lcdc_csr.rf(LCDSR_SIPSTS) != 0 {}
    }

    fn select_pwm_clock_source(&mut self, source: LcdcPwmClockSource) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG0_CLKPWMSEL, source as u32);
    }

    #[inline]
    pub fn set_clock_divider(&self, value: u8) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG0_CLKDIV, value.saturating_sub(2) as u32);
    }

    fn set_hsync_pulse_width(&self, value: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG1_HSPW, value as u32);
    }

    fn set_vsync_pulse_width(&self, value: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG1_VSPW, value as u32);
    }

    #[inline]
    pub fn set_layer_clock_gating_disable(&self, layer: LcdcLayerId, disable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(LCDCFG0_CGDISBASE, !disable as u32),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(LCDCFG0_CGDISOVR1, !disable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(LCDCFG0_CGDISOVR2, !disable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(LCDCFG0_CGDISHEO, !disable as u32),
        }
    }

    fn set_num_active_rows(&mut self, value: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG4_RPF, value.saturating_sub(1) as u32);
    }

    fn set_num_pixels_per_line(&mut self, value: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG4_PPL, value.saturating_sub(1) as u32);
    }

    fn set_display_guard_time(&mut self, frames: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG5_GUARDTIME, frames as u32);
    }

    fn set_output_mode(&mut self, mode: OutputColorMode) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG5_MODE, mode as u32);
    }

    fn set_display_signal_synchronization(&mut self, synchronous: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG5_DISPDLY, !synchronous as u32);
    }

    fn set_vsync_pulse_start(&mut self, edge: VsyncSyncEdge) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG5_VSPDLYS, edge as u32);
    }

    fn set_vsync_polarity(&mut self, polarity: SignalPolarity) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG5_VSPOL, polarity as u32);
    }

    fn set_hsync_polarity(&mut self, polarity: SignalPolarity) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG5_HSPOL, polarity as u32);
    }

    #[inline]
    pub fn set_pwm_compare_value(&self, value: u8) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG6_PWMCVAL, value as u32);
    }

    #[inline]
    pub fn pwm_compare_value(&self) -> u8 {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rf(LCDCFG6_PWMCVAL) as u8
    }

    fn set_pwm_signal_polarity(&mut self, polarity: SignalPolarity) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG6_PWMPOL, polarity as u32);
    }

    fn set_pwm_prescaler(&mut self, div: u8) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG6_PWMPS, div as u32);
    }

    fn set_pixel_clock_enable(&self, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        if enable {
            lcdc_csr.rmwf(LCDEN_CLKEN, 1);
        } else {
            lcdc_csr.rmwf(LCDDIS_CLKDIS, 1);
        }
    }

    fn wait_for_clock_running(&self) {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        while lcdc_csr.rf(LCDSR_CLKSTS) == 0 {}
    }

    fn wait_for_clock_stopped(&self) {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        while lcdc_csr.rf(LCDSR_CLKSTS) != 0 {}
    }

    fn set_sync_enable(&mut self, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        if enable {
            lcdc_csr.rmwf(LCDEN_SYNCEN, 1);
        } else {
            lcdc_csr.rmwf(LCDDIS_SYNCDIS, 1);
        }
    }

    fn wait_for_sync(&self) {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        while lcdc_csr.rf(LCDSR_LCDSTS) == 0 {}
    }

    fn wait_for_sync_off(&self) {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        while lcdc_csr.rf(LCDSR_LCDSTS) != 0 {}
    }

    fn set_disp_signal_enable(&mut self, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        if enable {
            lcdc_csr.rmwf(LCDEN_DISPEN, 1);
        } else {
            lcdc_csr.rmwf(LCDDIS_DISPDIS, 1);
        }
    }

    fn wait_for_disp_signal(&self) {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        while lcdc_csr.rf(LCDSR_DISPSTS) == 0 {}
    }

    fn wait_for_disp_off(&self) {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        while lcdc_csr.rf(LCDSR_DISPSTS) != 0 {}
    }

    fn set_pwm_enable(&self, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        if enable {
            lcdc_csr.rmwf(LCDEN_PWMEN, 1);
        } else {
            lcdc_csr.rmwf(LCDDIS_PWMDIS, 1);
        }
    }

    #[inline]
    pub fn set_window_size(&self, layer: LcdcLayerId, width: u16, height: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Ovr1 => {
                lcdc_csr.wo(OVR1CFG3, (((height - 1) as u32) << 16) | (width - 1) as u32)
            }
            LcdcLayerId::Ovr2 => {
                lcdc_csr.wo(OVR2CFG3, (((height - 1) as u32) << 16) | (width - 1) as u32)
            }
            LcdcLayerId::Heo => {
                lcdc_csr.wo(HEOCFG3, (((height - 1) as u32) << 16) | (width - 1) as u32)
            }
            LcdcLayerId::Base => (), // Unsupported
        }
    }

    #[inline]
    pub fn set_window_pos(&self, layer: LcdcLayerId, x: u16, y: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Ovr1 => lcdc_csr.wo(OVR1CFG2, ((y as u32) << 16) | x as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.wo(OVR2CFG2, ((y as u32) << 16) | x as u32),
            LcdcLayerId::Heo => lcdc_csr.wo(HEOCFG2, ((y as u32) << 16) | x as u32),
            LcdcLayerId::Base => (), // Unsupported
        }
    }

    #[inline]
    pub fn configure_heo(&self) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.wo(HEOCFG0, 0x00001130);
        self.set_rgb_mode_input(LcdcLayerId::Heo, DEFAULT_GFX_COLOR_MODE);
        lcdc_csr.wo(HEOCFG12, 0x00ff0020);
        lcdc_csr.wo(HEOCFG14, 0x00ff0020);
        lcdc_csr.wo(HEOCFG15, 0x7cde1c94);
        lcdc_csr.wo(HEOCFG16, 0x50200094);
    }

    #[inline]
    pub fn set_use_dma_path_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECFG4_DMA, enable as u32),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_DMA, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_DMA, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_DMA, enable as u32),
        }
    }

    #[inline]
    pub fn set_rgb_mode_input(&self, layer: LcdcLayerId, mode: ColorMode) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECFG1_RGBMODE, mode as u32),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG1_RGBMODE, mode as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG1_RGBMODE, mode as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG1_RGBMODE, mode as u32),
        }
    }

    #[inline]
    pub fn set_dma_address_register(&self, layer: LcdcLayerId, addr: u32) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.wo(BASEADDR, addr),
            LcdcLayerId::Ovr1 => lcdc_csr.wo(OVR1ADDR, addr),
            LcdcLayerId::Ovr2 => lcdc_csr.wo(OVR2ADDR, addr),
            LcdcLayerId::Heo => lcdc_csr.wo(HEOADDR, addr),
        }
    }

    #[inline]
    pub fn set_dma_head_pointer(&self, layer: LcdcLayerId, addr: u32) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.wo(BASEHEAD, addr),
            LcdcLayerId::Ovr1 => lcdc_csr.wo(OVR1HEAD, addr),
            LcdcLayerId::Ovr2 => lcdc_csr.wo(OVR2HEAD, addr),
            LcdcLayerId::Heo => lcdc_csr.wo(HEOHEAD, addr),
        }
    }

    #[inline]
    pub fn get_dma_head_pointer(&self, layer: LcdcLayerId) -> u32 {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rf(BASEHEAD_HEAD),
            LcdcLayerId::Ovr1 => lcdc_csr.rf(OVR1HEAD_HEAD),
            LcdcLayerId::Ovr2 => lcdc_csr.rf(OVR2HEAD_HEAD),
            LcdcLayerId::Heo => lcdc_csr.rf(HEOHEAD_HEAD),
        }
    }

    #[inline]
    pub fn set_dma_descriptor_next_address(&self, layer: LcdcLayerId, addr: u32) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASENEXT_NEXT, addr),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1NEXT_NEXT, addr),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2NEXT_NEXT, addr),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEONEXT_NEXT, addr),
        }
    }

    #[inline]
    pub fn get_dma_descriptor_next_address(&self, layer: LcdcLayerId) -> u32 {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rf(BASENEXT_NEXT),
            LcdcLayerId::Ovr1 => lcdc_csr.rf(OVR1NEXT_NEXT),
            LcdcLayerId::Ovr2 => lcdc_csr.rf(OVR2NEXT_NEXT),
            LcdcLayerId::Heo => lcdc_csr.rf(HEONEXT_NEXT),
        }
    }

    #[inline]
    pub fn set_transfer_descriptor_fetch_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECTRL_DFETCH, enable as u32),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CTRL_DFETCH, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CTRL_DFETCH, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCTRL_DFETCH, enable as u32),
        }
    }

    #[inline]
    pub fn set_system_bus_dma_burst_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECFG0_DLBO, enable as u32),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG0_DLBO, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG0_DLBO, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG0_DLBO, enable as u32),
        }
    }

    #[inline]
    pub fn set_system_bus_dma_burst_length(&self, layer: LcdcLayerId, len: BurstLength) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECFG0_BLEN, len as u32),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG0_BLEN, len as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG0_BLEN, len as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG0_BLEN, len as u32),
        }
    }

    #[inline]
    pub fn set_channel_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => {
                if enable {
                    lcdc_csr.rmwf(BASECHER_CHEN, 1);
                } else {
                    lcdc_csr.rmwf(BASECHDR_CHDIS, 1);
                }
            }
            LcdcLayerId::Ovr1 => {
                if enable {
                    lcdc_csr.rmwf(OVR1CHER_CHEN, 1);
                } else {
                    lcdc_csr.rmwf(OVR1CHDR_CHDIS, 1);
                }
            }
            LcdcLayerId::Ovr2 => {
                if enable {
                    lcdc_csr.rmwf(OVR2CHER_CHEN, 1);
                } else {
                    lcdc_csr.rmwf(OVR2CHDR_CHDIS, 1);
                }
            }
            LcdcLayerId::Heo => {
                if enable {
                    lcdc_csr.rmwf(HEOCHER_CHEN, 1);
                } else {
                    lcdc_csr.rmwf(HEOCHDR_CHDIS, 1);
                }
            }
        }
    }

    fn set_vertical_front_porch_width(&mut self, margin: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG2_VFPW, margin as u32);
    }

    fn set_vertical_back_porch_width(&mut self, margin: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG2_VBPW, margin as u32);
    }

    fn set_horizontal_front_porch_width(&mut self, margin: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG3_HFPW, margin as u32);
    }

    fn set_horizontal_back_porch_width(&mut self, margin: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(LCDCFG3_HBPW, margin as u32);
    }

    #[inline]
    pub fn update_overlay_attributes_enable(&self, layer: LcdcLayerId) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECHER_UPDATEEN, 1),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CHER_UPDATEEN, 1),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CHER_UPDATEEN, 1),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCHER_UPDATEEN, 1),
        }
    }

    #[inline]
    pub fn update_attribute(&self, layer: LcdcLayerId) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(ATTR_BASE, 1),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(ATTR_OVR1, 1),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(ATTR_OVR2, 1),
            LcdcLayerId::Heo => lcdc_csr.rmwf(ATTR_HEO, 1),
        }
    }

    #[inline]
    pub fn reset_channel(&self, layer: LcdcLayerId) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECHDR_CHRST, 1),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CHDR_CHRST, 1),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CHDR_CHRST, 1),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCHDR_CHRST, 1),
        }
    }

    #[inline]
    pub fn set_add_to_queue_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECHER_A2QEN, enable as u32),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CHER_A2QEN, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CHER_A2QEN, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCHER_A2QEN, enable as u32),
        }
    }

    #[inline]
    pub fn add_dma_desc_to_queue(&self, layer: LcdcLayerId) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(ATTR_BASEA2Q, 1),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(ATTR_OVR1A2Q, 1),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(ATTR_OVR2A2Q, 1),
            LcdcLayerId::Heo => lcdc_csr.rmwf(ATTR_HEOA2Q, 1),
        }
    }

    #[inline]
    pub fn is_dma_enabled(&self, layer: LcdcLayerId) -> bool {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rf(BASECFG4_DMA) != 0,
            LcdcLayerId::Ovr1 => lcdc_csr.rf(OVR1CFG9_DMA) != 0,
            LcdcLayerId::Ovr2 => lcdc_csr.rf(OVR2CFG9_DMA) != 0,
            LcdcLayerId::Heo => lcdc_csr.rf(HEOCFG12_DMA) != 0,
        }
    }

    #[inline]
    pub fn layer_interrupt_status(&self, layer: LcdcLayerId) -> LcdcLayerInterruptStatus {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        let field = match layer {
            LcdcLayerId::Base => BASEISR,
            LcdcLayerId::Ovr1 => OVR1ISR,
            LcdcLayerId::Ovr2 => OVR2ISR,
            LcdcLayerId::Heo => HEOISR,
        };

        LcdcLayerInterruptStatus::from_bits_retain(lcdc_csr.r(field))
    }

    #[inline]
    pub fn enable_dma_desc_loaded_interrupt(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        if enable {
            match layer {
                LcdcLayerId::Base => lcdc_csr.wfo(BASEIER_DSCR, 1),
                LcdcLayerId::Ovr1 => lcdc_csr.wfo(OVR1IER_DSCR, 1),
                LcdcLayerId::Ovr2 => lcdc_csr.wfo(OVR2IER_DSCR, 1),
                LcdcLayerId::Heo => lcdc_csr.wfo(HEOIER_DSCR, 1),
            }
        } else {
            match layer {
                LcdcLayerId::Base => lcdc_csr.wfo(BASEIDR_DSCR, 1),
                LcdcLayerId::Ovr1 => lcdc_csr.wfo(OVR1IDR_DSCR, 1),
                LcdcLayerId::Ovr2 => lcdc_csr.wfo(OVR2IDR_DSCR, 1),
                LcdcLayerId::Heo => lcdc_csr.wfo(HEOIDR_DSCR, 1),
            }
        }
    }

    #[inline]
    pub fn is_add_to_queue_pending(&self, layer: LcdcLayerId) -> bool {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        let field = match layer {
            LcdcLayerId::Base => BASECHSR_A2QSR,
            LcdcLayerId::Ovr1 => OVR1CHSR_A2QSR,
            LcdcLayerId::Ovr2 => OVR2CHSR_A2QSR,
            LcdcLayerId::Heo => HEOCHSR_A2QSR,
        };

        lcdc_csr.rf(field) != 0
    }

    #[inline]
    pub fn wait_for_next_frame(&self) {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);

        while lcdc_csr.rf(LCDISR_SOF) == 0 {}
    }

    #[inline]
    pub fn set_blender_overlay_layer_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_OVR, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_OVR, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_OVR, enable as u32),
        }
    }

    #[inline]
    pub fn set_blender_dma_layer_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_DMA, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_DMA, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_DMA, enable as u32),
        }
    }

    #[inline]
    pub fn set_blender_local_alpha_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_LAEN, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_LAEN, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_LAEN, enable as u32),
        }
    }

    #[inline]
    pub fn set_blender_global_alpha_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_GAEN, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_GAEN, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_GAEN, enable as u32),
        }
    }

    #[inline]
    pub fn set_blender_chroma_key_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_CRKEY, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_CRKEY, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_CRKEY, enable as u32),
        }
    }

    #[inline]
    pub fn set_blender_rev_alpha(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_REVALPHA, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_REVALPHA, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_REVALPHA, enable as u32),
        }
    }

    #[inline]
    pub fn set_blender_inv(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_INV, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_INV, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_INV, enable as u32),
        }
    }

    #[inline]
    pub fn set_blender_iterated_color_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_ITER2BL, enable as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_ITER2BL, enable as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_ITER2BL, enable as u32),
        }
    }

    #[inline]
    pub fn set_blender_use_iterated_color(&self, layer: LcdcLayerId, do_use: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_ITER, do_use as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_ITER, do_use as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_ITER, do_use as u32),
        }
    }

    #[inline]
    pub fn set_discard_area(&self, layer: LcdcLayerId, x: u16, y: u16, w: u16, h: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => {
                lcdc_csr.rmwf(BASECFG5_DISCXPOS, x as u32);
                lcdc_csr.rmwf(BASECFG5_DISCYPOS, y as u32);
                lcdc_csr.rmwf(BASECFG6_DISCXSIZE, w.saturating_sub(1) as u32);
                lcdc_csr.rmwf(BASECFG6_DISCYSIZE, h.saturating_sub(1) as u32);
            }
            _ => todo!(),
        }
    }

    #[inline]
    pub fn discard_area_enable(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECFG4_DISCEN, enable as u32),
            _ => todo!(),
        }
    }

    #[inline]
    pub fn set_horiz_stride(&self, layer: LcdcLayerId, xstride: i32) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECFG2_XSTRIDE, xstride as u32),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG4_XSTRIDE, xstride as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG4_XSTRIDE, xstride as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG5_XSTRIDE, xstride as u32),
        }
    }

    #[inline]
    pub fn set_pixel_stride(&self, layer: LcdcLayerId, pstride: i32) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG5_PSTRIDE, pstride as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG5_PSTRIDE, pstride as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG6_PSTRIDE, pstride as u32),
        }
    }

    #[inline]
    pub fn set_default_color(&self, layer: LcdcLayerId, r: u8, g: u8, b: u8) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        let color = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.wo(BASECFG3, color),
            LcdcLayerId::Ovr1 => lcdc_csr.wo(OVR1CFG6, color),
            LcdcLayerId::Ovr2 => lcdc_csr.wo(OVR2CFG6, color),
            LcdcLayerId::Heo => lcdc_csr.wo(HEOCFG9, color),
        }
    }

    #[inline]
    pub fn set_heo_mem_size(&self, w: u16, h: u16) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.wo(HEOCFG4, (((h - 1) as u32) << 16) | (w - 1) as u32);
    }

    #[inline]
    pub fn set_heo_on_top(&self, heo_on_top: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(HEOCFG12_VIDPRI, heo_on_top as u32);
    }

    /// Enables scaling and automatically calculates internal scaling factors based on the
    /// difference Between framebuffer size and the window size configured for the
    /// `HEO` layer.
    #[inline]
    pub fn set_heo_scaling(&self, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        let reg = if enable {
            let (xf, yf) = self.compute_scaling_factors();
            (1 << 31) | ((yf as u32) << 16) | xf as u32
        } else {
            0u32
        };

        lcdc_csr.wo(HEOCFG13, reg);
    }

    // SAMA5D2x datasheet, sections 38.6.9.2 and 38.6.9.3
    #[inline]
    pub fn compute_scaling_factors(&self) -> (u16, u16) {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);
        let xmemsize = lcdc_csr.rf(HEOCFG4_XMEMSIZE) as u16;
        let ymemsize = lcdc_csr.rf(HEOCFG4_YMEMSIZE) as u16;
        let xsize = lcdc_csr.rf(HEOCFG3_XSIZE) as u16;
        let ysize = lcdc_csr.rf(HEOCFG3_YSIZE) as u16;

        // Calculate in u32 to avoid overflow
        let xfactor_1st = ((2048_u32 * xmemsize as u32 / xsize as u32) + 1) as u16;
        let yfactor_1st = ((2048_u32 * ymemsize as u32 / ysize as u32) + 1) as u16;

        let xfactor = if (xfactor_1st * xsize / 2048) > xmemsize {
            xfactor_1st - 1
        } else {
            xfactor_1st
        };

        let yfactor = if (yfactor_1st * ysize / 2048) > ymemsize {
            yfactor_1st - 1
        } else {
            yfactor_1st
        };

        (xfactor, yfactor)
    }

    #[inline]
    pub fn blender_set_global_alpha(&self, layer: LcdcLayerId, alpha: u8) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG9_GA, alpha as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG9_GA, alpha as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG12_GA, alpha as u32),
        }
    }

    #[inline]
    pub fn set_rotation_optimization_dis(&self, layer: LcdcLayerId, rotdis: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG0_ROTDIS, rotdis as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG0_ROTDIS, rotdis as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG0_ROTDIS, rotdis as u32),
        }
    }

    #[inline]
    pub fn set_lock_dis(&self, layer: LcdcLayerId, lockdis: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => (), // Unsupported
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG0_LOCKDIS, lockdis as u32),
            LcdcLayerId::Ovr2 => lcdc_csr.rmwf(OVR2CFG0_LOCKDIS, lockdis as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG0_LOCKDIS, lockdis as u32),
        }
    }

    #[inline]
    pub fn set_sif(&self, layer: LcdcLayerId, sif: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        match layer {
            LcdcLayerId::Base => lcdc_csr.rmwf(BASECFG0_SIF, sif as u32),
            LcdcLayerId::Ovr1 => lcdc_csr.rmwf(OVR1CFG0_SIF, sif as u32),
            LcdcLayerId::Ovr2 => (), /* FIXME: OVR2CFG0_SIF not found in generated utralib: */
            // lcdc_csr.rmwf(OVR2CFG0_SIF, sif as u32),
            LcdcLayerId::Heo => lcdc_csr.rmwf(HEOCFG0_SIF, sif as u32),
        }
    }

    #[inline]
    pub fn interrupt_status(&self) -> LcdcInterruptStatus {
        let lcdc_csr = CSR::new(self.base_addr as *mut u32);
        LcdcInterruptStatus::from_bits_retain(lcdc_csr.r(LCDISR))
    }

    #[inline]
    pub fn set_heo_downscale_opt(&self, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        lcdc_csr.rmwf(HEOCFG1_DSCALEOPT, !enable as u32);
    }

    #[inline]
    pub fn enable_start_of_frame_interrupt(&self, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);
        if enable {
            lcdc_csr.wfo(LCDIER_SOFIE, 1);
        } else {
            lcdc_csr.wfo(LCDIDR_SOFID, 1);
        }
    }

    #[inline]
    pub fn enable_layer_interrupts(&self, layer: LcdcLayerId, enable: bool) {
        let mut lcdc_csr = CSR::new(self.base_addr as *mut u32);

        if enable {
            match layer {
                LcdcLayerId::Base => lcdc_csr.wfo(LCDIER_BASEIE, 1),
                LcdcLayerId::Ovr1 => lcdc_csr.wfo(LCDIER_OVR1IE, 1),
                LcdcLayerId::Ovr2 => lcdc_csr.wfo(LCDIER_OVR2IE, 1),
                LcdcLayerId::Heo => lcdc_csr.wfo(LCDIER_HEOIE, 1),
            }
        } else {
            match layer {
                LcdcLayerId::Base => lcdc_csr.wfo(LCDIDR_BASEID, 1),
                LcdcLayerId::Ovr1 => lcdc_csr.wfo(LCDIDR_OVR1ID, 1),
                LcdcLayerId::Ovr2 => lcdc_csr.wfo(LCDIDR_OVR2ID, 1),
                LcdcLayerId::Heo => lcdc_csr.wfo(LCDIDR_HEOID, 1),
            }
        }
    }
}
