use {
    bitflags::bitflags,
    utralib::{utra::isc::*, CSR, HW_ISC_BASE},
};

bitflags! {
    #[derive(Debug, Copy, Clone)]
    pub struct ISCStatus: u32 {
        /// Vertical Synchronization Detected Interrupt (cleared on read)
        const VD       = 1 << 0;
        /// Horizontal Synchronization Detected Interrupt (cleared on read)
        const HD       = 1 << 1;
        /// Software Reset Completed Interrupt (cleared on read)
        const SWRST    = 1 << 4;
        /// Disable Completed Interrupt (cleared on read)
        const DIS      = 1 << 5;
        /// DMA Done Interrupt (cleared on read)
        const DDONE    = 1 << 8;
        /// DMA List Done Interrupt (cleared on read)
        const LDONE    = 1 << 9;
        /// Histogram Completed Interrupt (cleared on read)
        const HISDONE  = 1 << 12;
        /// Histogram Clear Interrupt (cleared on read)
        const HISCLR   = 1 << 13;
        /// Write Channel Error Interrupt (cleared on read)
        const WERR     = 1 << 16;
        /// Write Channel Error Identifier 0 (cleared on read)
        const WERRID1  = 1 << 18;
        /// Write Channel Error Identifier 1 (cleared on read)
        const WERRID0  = 1 << 17;
        /// Read Channel Error Interrupt (cleared on read)
        const RERR     = 1 << 20;
        /// Vertical Front Porch Overflow Interrupt (cleared on read)
        const VFPOV    = 1 << 24;
        /// Data Overflow Interrupt (cleared on read)
        const DAOV     = 1 << 25;
        /// Vertical Synchronization Timeout Interrupt (cleared on read)
        const VDTO     = 1 << 26;
        /// Horizontal Synchronization Timeout Interrupt (cleared on read)
        const HDTO     = 1 << 27;
        /// CCIR Decoder Error Interrupt (cleared on read)
        const CCIRERR  = 1 << 28;
    }
}

/// DMA Memory Burst Size
#[derive(Debug)]
#[allow(dead_code)]
enum DMABurstSize {
    /// DMA single access
    Single = 0,
    /// 4-beat burst access
    Beats4,
    /// 8-beat burst access
    Beats8,
    /// 16-beat burst access
    Beats16,
}

/// DMA Input Mode Selection
#[derive(Debug)]
#[allow(dead_code)]
enum DmaInputMode {
    /// 8 bits, single channel packed
    Packed8 = 0,
    /// 16 bits, single channel packed
    Packed16,
    /// 32 bits, single channel packed
    Packed32,
    /// 32 bits, dual channel
    YC422Sp,
    /// 32 bits, triple channel
    Yc422P,
    /// 32 bits, dual channel
    YC420Sp,
    /// 32 bits, triple channel
    YC420P,
}

#[derive(Debug)]
#[allow(dead_code)]
enum RlpMode {
    /// 8-bit data.
    Dat8 = 0,
    /// 9-bit data
    Dat9 = 1,
    /// 10-bit data
    Dat10 = 2,
    /// 11-bit data
    Dat11 = 3,
    /// 12-bit data
    Dat12 = 4,
    /// 8-bit luminance only
    DatY8 = 5,
    /// 10-bit luminance only
    DatY10 = 6,
    /// 12-bit RGB+4-bit Alpha (MSB)
    Argb444 = 7,
    /// 15-bit RGB+1-bit Alpha (MSB)
    Argb555 = 8,
    /// 16-bit RGB
    Rgb565 = 9,
    /// 24-bits RGB mode+8-bit Alpha
    Argb32 = 10,
    /// YCbCr mode (full range, 0–255)
    YyCc = 11,
    /// YCbCr mode (limited range)
    YyCcLimitedYCbCr = 12,
}

#[derive(Debug, Copy, Clone)]
enum Polarity {
    /// Signal is active high, i.e. valid pixels are sampled when the signal is asserted.
    High = 0,

    /// Signal is active low, i.e. valid pixels are sampled when the signal is deasserted.
    Low = 1,
}

#[derive(Debug)]
#[allow(dead_code)]
enum PfeBps {
    Twelve = 0,
    Eleven = 1,
    Ten = 2,
    Nine = 3,
    Eight = 4,
}

#[derive(Debug)]
#[allow(dead_code)]
enum PfeVideoMode {
    /// Video source is progressive.
    Progressive = 0,
    /// Video source is interlaced, two fields are captured starting with top field.
    DfTop = 1,
    /// Video source is interlaced, two fields are captured starting with bottom field.
    DfBottom = 2,
    /// Video source is interlaced, two fields are captured immediately.
    DfImmediate = 3,
    /// Video source is interlaced, one field is captured starting with the top field.
    SfTop = 4,
    /// Video source is interlaced, one field is captured starting with the bottom field.
    SfBottom = 5,
    /// Video source is interlaced, one field is captured starting immediately.
    SfImmediate = 6,
}

#[derive(Debug)]
pub enum BayerPattern {
    GrGr = 0,
    RgRg = 1,
    GbGb = 2,
    BgBg = 3,
}

#[derive(Debug)]
pub enum CcirByteOrder {
    /// CBY Byte ordering Cb0, Y0, Cr0, Y1
    Cby = 0,
    /// CRY Byte ordering Cr0, Y0, Cb0, Y1
    Cry = 1,
    /// YCB Byte ordering Y0, Cb0, Y1, Cr0
    Ycb = 2,
    /// YCR Byte ordering Y0, Cr0, Y1, Cb0
    Ycr = 3,
}

// Implement above structure in Rust
#[derive(Debug)]
#[allow(dead_code)]
pub struct ColorCorrectionConfig {
    r_offset: u16,
    g_offset: u16,
    b_offset: u16,
    rr_gain: u16,
    rg_gain: u16,
    rb_gain: u16,
    gg_gain: u16,
    gr_gain: u16,
    gb_gain: u16,
    bg_gain: u16,
    br_gain: u16,
    bb_gain: u16,
}

impl Default for ColorCorrectionConfig {
    fn default() -> Self {
        ColorCorrectionConfig {
            r_offset: 0,
            g_offset: 0,
            b_offset: 0,
            rr_gain: 0x100,
            rg_gain: 0,
            rb_gain: 0,
            gg_gain: 0x100,
            gr_gain: 0,
            gb_gain: 0,
            bg_gain: 0,
            br_gain: 0,
            bb_gain: 0x100,
        }
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub enum DmaDescriptorView {
    #[default]
    Packed = 0,
    SemiPlanar = 1,
    Planar = 2,
}

/// DMA descriptor control configuration.
#[derive(Debug, Default)]
pub struct DmaControlConfig {
    /// Enable DMA descriptor
    pub descriptor_enable: bool,

    /// Descriptor View : Packed, semi-planar, planar
    pub descriptor_view: DmaDescriptorView,

    /// DMA Done interrupt enable
    pub interrupt_enable: bool,

    /// Write back operation is enabled
    pub writeback_enable: bool,

    /// Value of Captured Frame Field Signal
    pub field: bool,

    /// Descriptor Processing Status
    pub done: bool,
}

/// DMA view descriptor. Must be aligned in memory at 32-bit boundary.
#[derive(Debug, Copy, Clone)]
#[repr(C, align(32))]
pub struct DmaView {
    /// ISC DMA Control
    ctrl: u32,
    /// Next ISC DMA Descriptor Address number
    next_desc: u32,
    /// Transfer Address
    addr: u32,
    // Stride
    stride: u32,
}

impl DmaView {
    pub const fn new() -> DmaView {
        DmaView {
            ctrl: 0,
            next_desc: 0,
            addr: 0,
            stride: 0,
        }
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(u8)]
pub enum ClkSel {
    Hclock = 0,
    Isclk,
    Gck,
}

#[derive(Debug, Copy, Clone)]
pub struct DmaBuffer {
    pub dma_desc_addr: u32,
    pub dma_desc_phys_addr: u32,
    pub fb_phys_addr: u32,
}

impl DmaBuffer {
    #[inline]
    pub fn new(dma_desc_addr: u32, dma_desc_phys_addr: u32, fb_phys_addr: u32) -> Self {
        DmaBuffer {
            dma_desc_addr,
            dma_desc_phys_addr,
            fb_phys_addr,
        }
    }

    /// Applies DMA descriptor to the memory address located at `dma_desc_addr`.
    /// Optionally sets the next descriptor address or loops the descriptor on itself.
    fn write_dma_descriptor(&self, next: Option<&DmaBuffer>) {
        // Loop the descriptor on itself it there's no next descriptor
        let next_desc_phys_addr = next
            .map(|d| d.dma_desc_phys_addr)
            .unwrap_or(self.dma_desc_phys_addr);

        let dma_desc_1 = self.dma_desc_addr as *mut DmaView;
        unsafe {
            (*dma_desc_1).ctrl = 0b01; // Mode = packed (0), descriptor enable = 1
            (*dma_desc_1).next_desc = next_desc_phys_addr;
            (*dma_desc_1).addr = self.fb_phys_addr;
            (*dma_desc_1).stride = 0;
        }
    }
}

pub struct Isc {
    base_addr: u32,
}

#[allow(dead_code)]
impl Isc {
    #[inline]
    pub fn new() -> Self {
        Isc {
            base_addr: HW_ISC_BASE as u32,
        }
    }

    #[inline]
    pub fn with_alt_base_addr(base_addr: u32) -> Self {
        Isc { base_addr }
    }

    #[inline]
    pub fn setup_clocks(&mut self, mck_div: u8, mck_sel: ClkSel, ic_div: u8, ic_sel: ClkSel) {
        let mut csr = CSR::new(self.base_addr as *mut u32);

        // Configure master clock
        csr.rmwf(CLKCFG_MCSEL, mck_sel as u32);
        csr.rmwf(CLKCFG_MCDIV, mck_div as u32);

        // Configure ISP clock
        csr.rmwf(CLKCFG_ICDIV, ic_div as u32);
        csr.rmwf(CLKCFG_ICSEL, ic_sel as u32);
    }

    #[inline]
    pub fn enable_clock(&mut self) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        self.wait_for_sync_clk();
        // Both clocks in one write, because the second would land while the first is still
        // synchronizing across clock domains, which is forbidden (52.5.1.1). The master
        // clock is what provides the cam chip with its clock.
        let reg = csr.ms(CLKEN_MCEN, 1) | csr.ms(CLKEN_ICEN, 1);
        csr.wo(CLKEN, reg);

        // The status below reads the pre-write state until the synchronization is over.
        self.wait_for_sync_clk();

        assert_ne!(csr.rf(CLKSR_MCSR), 0);
        assert_ne!(csr.rf(CLKSR_ICSR), 0);
    }

    #[inline]
    pub fn disable_clock(&mut self) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        self.wait_for_sync_clk();
        // Both clocks in one write, because the second would land while the first is still
        // synchronizing across clock domains, which is forbidden (52.5.1.1).
        let reg = csr.ms(CLKDIS_ICDIS, 1) | csr.ms(CLKDIS_MCDIS, 1);
        csr.wo(CLKDIS, reg);

        // Returns with the clocks off, not merely requested off.
        self.wait_for_sync_clk();
    }

    fn wait_for_sync(&self) {
        let csr = CSR::new(self.base_addr as *mut u32);
        while csr.rf(CTRLSR_SIP) != 0 {
            armv7::asm::nop();
        }
    }

    fn wait_for_sync_clk(&self) {
        let csr = CSR::new(self.base_addr as *mut u32);
        while csr.rf(CLKSR_SIP) != 0 {
            armv7::asm::nop();
        }
    }

    fn set_dma_desc_phys_addr(&mut self, dma_desc_phys_addr: u32) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wo(DNDA, dma_desc_phys_addr);
    }

    #[inline]
    pub fn configure(&mut self, is_continuous: bool) {
        self.reset();

        self.pfe_set_continuous_mode(is_continuous);
        self.pfe_set_pclk_gated(true);

        // Configure the Parallel Front End module performs data
        // re-sampling across clock domain boundary. The PFE module
        // outputs a 12-bit data on the vp_data[11:0] bus
        self.pfe_set_video_mode(PfeVideoMode::Progressive);

        // Set 8 bit per sample as per schematic.
        self.pfe_set_bps(PfeBps::Eight);

        // Set HSYNC and VSYNC signal polarity
        let vpol = Polarity::Low;
        let hpol = Polarity::High;
        self.pfe_set_sync_polarity(vpol, hpol);

        // Set pixel clock polarity
        self.pfe_set_pclk_polarity(Polarity::High);

        // Set color output mode and alpha value
        self.rlp_configure(RlpMode::Dat8, 0xff);

        // Update profile
        self.update_profile();
    }

    pub fn set_cropping_area(&mut self, x: u32, y: u32, width: u32, height: u32) {
        self.wait_for_sync();
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(PFE_CFG0_COLEN, 1);
        csr.rmwf(PFE_CFG0_ROWEN, 1);
        csr.rmwf(PFE_CFG1_COLMIN, x * 2);
        csr.rmwf(PFE_CFG1_COLMAX, (x + width) * 2 - 1);
        csr.rmwf(PFE_CFG2_ROWMIN, y);
        csr.rmwf(PFE_CFG2_ROWMAX, y + height - 1);
    }

    #[inline]
    pub fn start_capture(&mut self) {
        self.wait_for_sync();
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(CTRLEN_CAPTURE, 1);
    }

    #[inline]
    pub fn stop_capture(&mut self) {
        self.wait_for_sync();
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(CTRLDIS_DISABLE, 1);
    }

    #[inline]
    pub fn interrupt_status(&mut self) -> ISCStatus {
        let csr = CSR::new(self.base_addr as *mut u32);
        ISCStatus::from_bits_truncate(csr.r(INTSR))
    }

    #[inline]
    pub fn enable_interrupt(&mut self, isr: ISCStatus) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wo(INTEN, isr.bits());
    }

    #[inline]
    pub fn disable_interrupt(&mut self, isr: ISCStatus) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wo(INTDIS, isr.bits());
    }

    #[inline]
    pub fn configure_dma(
        &mut self,
        dma_buffers: &[DmaBuffer],
        dma_control_config: &DmaControlConfig,
        cache_maintenance: impl Fn(),
    ) {
        // Build a linked list of DMA descriptors
        let first_dma_buffer = dma_buffers.first().expect("empty DMA buffer list");
        for desc_pair in dma_buffers.windows(2) {
            desc_pair[0].write_dma_descriptor(Some(&desc_pair[1]));
        }
        if let Some(last_desc) = dma_buffers.iter().last() {
            last_desc.write_dma_descriptor(Some(first_dma_buffer));
        }

        cache_maintenance();

        // Configure the beginning of the descriptor list
        self.set_dma_desc_phys_addr(first_dma_buffer.dma_desc_phys_addr);
        self.dma_configure_register(
            DmaInputMode::Packed8,
            DMABurstSize::Beats16,
            DMABurstSize::Single,
        );
        self.dma_enable(dma_control_config);
    }

    fn update_profile(&mut self) {
        self.wait_for_sync();
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(CTRLEN_UPPRO, 1);

        // Wait for the profile update to take place
        while csr.rf(CTRLSR_UPPRO) != 0 {
            armv7::asm::nop();
        }
    }

    fn dma_enable(&mut self, dma_control_config: &DmaControlConfig) {
        let mut csr = CSR::new(self.base_addr as *mut u32);

        csr.rmwf(DCTRL_DE, dma_control_config.descriptor_enable as u32);
        csr.rmwf(DCTRL_DVIEW, dma_control_config.descriptor_view as u32);
        csr.rmwf(DCTRL_IE, dma_control_config.interrupt_enable as u32);
        csr.rmwf(DCTRL_WB, dma_control_config.writeback_enable as u32);
        csr.rmwf(DCTRL_FIELD, dma_control_config.field as u32);
        csr.rmwf(DCTRL_DONE, dma_control_config.done as u32);
    }

    fn dma_configure_register(
        &mut self,
        imode: DmaInputMode,
        ymbsize: DMABurstSize,
        cmbsize: DMABurstSize,
    ) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(DCFG_IMODE, imode as u32);
        csr.rmwf(DCFG_YMBSIZE, ymbsize as u32);
        csr.rmwf(DCFG_CMBSIZE, cmbsize as u32);
    }

    fn cfa_enable(&mut self, enable: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(CFA_CTRL_ENABLE, enable as u32);
    }

    fn cfa_configure(&mut self, pattern: BayerPattern, interpolate_edges: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(CFA_CFG_BAYCFG, pattern as u32);
        csr.rmwf(CFA_CFG_EITPOL, interpolate_edges as u32);
    }

    fn gamma_enable(&mut self, enable: bool, r: bool, g: bool, b: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        if enable {
            csr.rmwf(GAM_CTRL_ENABLE, 1);
            csr.rmwf(GAM_CTRL_RENABLE, r as u32);
            csr.rmwf(GAM_CTRL_GENABLE, g as u32);
            csr.rmwf(GAM_CTRL_BENABLE, b as u32);
        } else {
            csr.rmwf(GAM_CTRL_ENABLE, 0);
            csr.rmwf(GAM_CTRL_RENABLE, 0);
            csr.rmwf(GAM_CTRL_GENABLE, 0);
            csr.rmwf(GAM_CTRL_BENABLE, 0);
        }
    }

    fn wb_enable(&mut self, enable: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(WB_CTRL_ENABLE, enable as u32);
    }

    fn wb_set_bayer_pattern(&mut self, pattern: BayerPattern) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(WB_CFG_BAYCFG, pattern as u32);
    }

    fn wb_adjust_color_offset(&mut self, b: u16, gb: u16, r: u16, gr: u16) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(WB_O_BGB_BOFST, b as u32);
        csr.wfo(WB_O_BGB_GBOFST, gb as u32);
        csr.wfo(WB_O_RGR_ROFST, r as u32);
        csr.wfo(WB_O_RGR_GROFST, gr as u32);
    }

    fn wb_adjust_color_gain(&mut self, b: u16, gb: u16, r: u16, gr: u16) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(WB_G_BGB_BGAIN, b as u32);
        csr.wfo(WB_G_BGB_GBGAIN, gb as u32);
        csr.wfo(WB_G_RGR_RGAIN, r as u32);
        csr.wfo(WB_G_RGR_GRGAIN, gr as u32);
    }

    fn cbc_enable(&mut self, enable: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(CBC_CTRL_ENABLE, enable as u32);
    }

    fn cbc_configure(
        &mut self,
        stream_enable: bool,
        byte_order: CcirByteOrder,
        brightness: u16,
        contrast: u16,
    ) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        if stream_enable {
            csr.rmwf(CBC_CFG_CCIR, 1);
            csr.rmwf(CBC_CFG_CCIRMODE, byte_order as u32);
        } else {
            csr.wfo(CBC_CFG_CCIR, 0);
        }

        csr.wo(CBC_BRIGHT, brightness as u32);
        csr.wo(CBC_CONTRAST, contrast as u32);
    }

    fn cc_enable(&mut self, enable: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(CC_CTRL_ENABLE, enable as u32);
    }

    fn cc_configure(&mut self, config: &ColorCorrectionConfig) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(CC_RR_RG_RRGAIN, config.rr_gain as u32);
        csr.rmwf(CC_RR_RG_RGGAIN, config.rg_gain as u32);
        csr.rmwf(CC_RB_OR_RBGAIN, config.rb_gain as u32);
        csr.rmwf(CC_RB_OR_ROFST, config.r_offset as u32);

        csr.rmwf(CC_GR_GG_GRGAIN, config.gr_gain as u32);
        csr.rmwf(CC_GR_GG_GGGAIN, config.gg_gain as u32);
        csr.rmwf(CC_GB_OG_GBGAIN, config.gb_gain as u32);
        csr.rmwf(CC_GB_OG_ROFST, config.g_offset as u32);

        csr.rmwf(CC_BR_BG_BRGAIN, config.br_gain as u32);
        csr.rmwf(CC_BR_BG_BGGAIN, config.bg_gain as u32);
        csr.rmwf(CC_BB_OB_BBGAIN, config.bb_gain as u32);
        csr.rmwf(CC_BB_OB_BOFST, config.b_offset as u32);
    }

    /// Configures Rounding, Limiting and Packing Mode.
    fn rlp_configure(&mut self, mode: RlpMode, alpha: u8) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(RLP_CFG_MODE, mode as u32);
        csr.rmwf(RLP_CFG_ALPHA, alpha as u32);
    }

    fn pfe_set_ccir656(&mut self, set: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(PFE_CFG0_CCIR656, set as u32);
    }

    fn pfe_set_ccir656_10bit(&mut self, set: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(PFE_CFG0_CCIR10_8N, set as u32);
    }

    fn pfe_set_pclk_polarity(&mut self, pol: Polarity) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(PFE_CFG0_PPOL, pol as u32);
    }

    fn pfe_set_sync_polarity(&mut self, vpol: Polarity, hpol: Polarity) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(PFE_CFG0_VPOL, vpol as u32);
        csr.rmwf(PFE_CFG0_HPOL, hpol as u32);
    }

    fn pfe_set_bps(&mut self, bps: PfeBps) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(PFE_CFG0_BPS, bps as u32);
    }

    fn pfe_set_video_mode(&mut self, video_mode: PfeVideoMode) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(PFE_CFG0_MODE, video_mode as u32);
    }

    /// Set PFE (Parallel Front End) in continuous mode.
    fn pfe_set_continuous_mode(&mut self, enable: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(PFE_CFG0_CONT, enable as u32);
    }

    /// Set PFE (Parallel Front End) pixel clock in gated mode
    fn pfe_set_pclk_gated(&mut self, gated: bool) {
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.rmwf(PFE_CFG0_GATED, gated as u32);
    }

    fn reset(&mut self) {
        self.wait_for_sync();
        let mut csr = CSR::new(self.base_addr as *mut u32);
        csr.wfo(CTRLDIS_SWRST, 1);
    }
}
