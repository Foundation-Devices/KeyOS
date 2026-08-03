mod avalanche;

use atsama5d27::{
    adc::Adc,
    trng::{Enabled, StatefulTrng, Trng as TrngDev},
};
use avalanche::{word_from_digest, AvalancheNoiseRng};
use sha2::{Digest, Sha256};
use trng::TrngSource;
use utralib::generated::*;
use xous::MemoryFlags;

/// Domain separator for the multi-source extractor.
const COMBINED_DOMAIN: &[u8] = b"keyos-trng-combined-v1";

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
            TrngSource::Combined => self.fill_combined(data),
        }
    }

    pub fn fill_avalanche_raw_samples(&mut self, data: &mut [u32]) { self.avalanche.fill_raw_samples(data); }

    /// Extract the avalanche noise via SHA-256, then XOR in one raw MCU TRNG
    /// word. XORing a word known to be uniform and independent of the
    /// avalanche samples (the MCU peripheral is a physically separate
    /// circuit) onto anything preserves that word's entropy exactly,
    /// regardless of the other operand's distribution -- unlike folding both
    /// raw sources into one hash and truncating to a word, which is not
    /// guaranteed to be a bijection and can lose a fraction of a bit even
    /// when the un-hashed operand is perfectly uniform. So Combined stays at
    /// least as strong as the MCU source alone whenever it's healthy, with no
    /// assumption needed about avalanche.
    fn fill_combined(&mut self, data: &mut [u32]) {
        if data.is_empty() {
            return;
        }

        let Self { trng, avalanche } = self;
        avalanche.with_adc(|noise| {
            for word in data {
                let mut hasher = Sha256::new();
                hasher.update(COMBINED_DOMAIN);
                noise.hash_samples(&mut hasher);
                *word = word_from_digest(hasher) ^ trng.read_u32();
            }
        });
    }
}
