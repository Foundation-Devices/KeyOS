//! Analog to digital converter (ADC) driver.

use {
    bitflags::bitflags,
    utralib::{utra::adc::*, HW_ADC_BASE, *},
};

const WPKEY: u32 = 0x414443; // "ADC"

bitflags! {
    #[derive(Debug, Copy, Clone)]
    pub struct ADCStatus: u32 {
        /// Data Ready (automatically set / cleared)
        const DRDY = 1 << 24;
    }
}

#[derive(Debug, Copy, Clone)]
pub enum AdcChannel {
    Channel0 = 0,
    Channel1 = 1,
    Channel2 = 2,
    Channel3 = 3,
    Channel4 = 4,
    Channel5 = 5,
    Channel6 = 6,
    Channel7 = 7,
    Channel8 = 8,
    Channel9 = 9,
    Channel10 = 10,
    Channel11 = 11,
}

#[derive(Debug, Copy, Clone)]
pub enum StartupTime {
    StartupTime0 = 0,
    StartupTime8 = 1,
    StartupTime16 = 2,
    StartupTime24 = 3,
    StartupTime64 = 4,
    StartupTime80 = 5,
    StartupTime96 = 6,
    StartupTime112 = 7,
    StartupTime512 = 8,
    StartupTime576 = 9,
    StartupTime640 = 10,
    StartupTime704 = 11,
    StartupTime768 = 12,
    StartupTime832 = 13,
    StartupTime896 = 14,
    StartupTime960 = 15,
}

pub struct Adc {
    base_addr: u32,
}

impl Default for Adc {
    fn default() -> Self {
        Adc::new()
    }
}

impl Adc {
    #[inline]
    pub fn new() -> Self {
        Self {
            base_addr: HW_ADC_BASE as u32,
        }
    }

    /// Creates ADC instance with a different base address. Used with virtual memory
    #[inline]
    pub fn with_alt_base_addr(base_addr: u32) -> Self {
        Self { base_addr }
    }

    #[inline]
    pub fn reset(&self) {
        let mut adc_csr = CSR::new(self.base_addr as *mut u32);
        adc_csr.wfo(CR_SWRST, 1);
        // The datasheet requires TRANSFER=2; this is also ADC_MR's reset value.
        adc_csr.wfo(MR_TRANSFER, 2);
    }

    #[inline]
    pub fn unlock(&self) {
        let mut adc_csr = CSR::new(self.base_addr as *mut u32);
        adc_csr.wfo(WPMR_WPKEY, WPKEY);
    }

    #[inline]
    pub fn set_prescaler(&self, psc: u8) {
        let mut adc_csr = CSR::new(self.base_addr as *mut u32);
        adc_csr.rmwf(MR_PRESCAL, psc as u32);
    }

    #[inline]
    pub fn set_startup_time(&self, sut: StartupTime) {
        let mut adc_csr = CSR::new(self.base_addr as *mut u32);
        adc_csr.rmwf(MR_STARTUP, sut as u32);
    }

    #[inline]
    pub fn status(&self) -> ADCStatus {
        let adc_csr = CSR::new(self.base_addr as *mut u32);
        ADCStatus::from_bits_truncate(adc_csr.r(ISR))
    }

    #[inline]
    pub fn start(&self) {
        let mut adc_csr = CSR::new(self.base_addr as *mut u32);
        adc_csr.wfo(CR_START, 1);
    }

    #[inline]
    pub fn is_data_ready(&self) -> bool {
        let adc_csr = CSR::new(self.base_addr as *mut u32);
        adc_csr.rf(ISR_DRDY) != 0
    }

    #[inline]
    pub fn enable_channel(&self, ch: AdcChannel) {
        let mut adc_csr = CSR::new(self.base_addr as *mut u32);
        adc_csr.wo(CHER, 1 << ch as u32);
    }

    #[inline]
    pub fn disable_channel(&self, ch: AdcChannel) {
        let mut adc_csr = CSR::new(self.base_addr as *mut u32);
        adc_csr.wo(CHDR, 1 << ch as u32);
    }

    fn wait_eoc(&self, ch: AdcChannel) {
        let adc_csr = CSR::new(self.base_addr as *mut u32);

        let eoc_bit = 1 << ch as u32;
        while adc_csr.r(ISR) & eoc_bit == 0 {
            armv7::asm::nop();
        }
    }

    #[inline]
    pub fn read(&self, ch: AdcChannel) -> u16 {
        self.wait_eoc(ch);

        const ADC_CDR_OFFSET: u32 = 0x50;
        let ptr = (self.base_addr + ADC_CDR_OFFSET) as *mut u32;
        let data = unsafe { ptr.add(ch as usize).read_volatile() } & 0xFFF;
        data as u16
    }

    #[inline]
    pub fn sleep(&self) {
        let mut adc_csr = CSR::new(self.base_addr as *mut u32);
        adc_csr.rmwf(MR_SLEEP, 1);
    }
}
