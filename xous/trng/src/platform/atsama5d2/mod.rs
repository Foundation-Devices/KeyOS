mod avalanche;

use atsama5d27::{
    adc::Adc,
    trng::{Enabled, StatefulTrng, Trng as TrngDev},
};
use avalanche::AvalancheNoiseRng;
use trng::TrngSource;
use utralib::generated::*;
use xous::MemoryFlags;

pub struct Trng {
    trng: StatefulTrng<Enabled>,
    avalanche: AvalancheNoiseRng,
}

impl Trng {
    pub fn new() -> Self {
        let trng_mem = xous::syscall::map_memory(
            xous::MemoryAddress::new(utra::trng::HW_TRNG_BASE),
            None,
            4096,
            MemoryFlags::W | MemoryFlags::DEV,
        )
        .expect("couldn't map TRNG peripheral");

        let adc_mem = xous::syscall::map_memory(
            xous::MemoryAddress::new(utra::adc::HW_ADC_BASE),
            None,
            4096,
            MemoryFlags::W | MemoryFlags::DEV,
        )
        .expect("couldn't map ADC peripheral");

        let trng = TrngDev::with_alt_base_addr(trng_mem.as_ptr() as u32).enable();
        let adc = Adc::with_alt_base_addr(adc_mem.as_ptr() as u32);
        let avalanche = AvalancheNoiseRng::new(adc);
        Trng { trng, avalanche }
    }

    pub fn fill_buf(&mut self, data: &mut [u32], source: TrngSource) {
        match source {
            TrngSource::Avalanche => self.avalanche.fill_buf(data),
            TrngSource::Mcu => {
                for word in data {
                    *word = self.trng.read_u32();
                }
            }
            TrngSource::Combined => {
                self.avalanche.fill_buf(data);
                for word in data {
                    *word ^= self.trng.read_u32();
                }
            }
        }
    }

    pub fn fill_avalanche_raw_samples(&mut self, data: &mut [u32]) { self.avalanche.fill_raw_samples(data); }
}
