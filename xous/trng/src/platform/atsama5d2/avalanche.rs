use atsama5d27::adc::{Adc, AdcChannel, StartupTime};
use sha2::{Digest, Sha256};

// Peripheral clock is around 80Mhz, max ADC clock is 20Mhz.
// The actual prescale divider is (this value + 1) * 2, so we
// are still well below the threshold.
const ADC_CLOCK_PRESCALER: u8 = 4;
const ADC_STARTUP_TIME: StartupTime = StartupTime::StartupTime24;
const NOISE_CHANNEL: AdcChannel = AdcChannel::Channel5;

// Number of raw 12-bit ADC samples hashed per 32-bit output word. SHA-256 removes
// the linear structure of the former XOR fold; the sample count is based on the
// measured entropy from the avalanche source with margin over a 32-bit output.
const RAW_SAMPLES_PER_WORD: usize = 16;

pub(crate) struct AvalancheNoiseRng {
    adc: Adc,
}

impl AvalancheNoiseRng {
    pub(crate) fn new(adc: Adc) -> Self { Self { adc } }

    pub(crate) fn fill_buf(&self, data: &mut [u32]) {
        if data.is_empty() {
            return;
        }

        self.with_adc(|noise| {
            for word in data {
                let mut hasher = Sha256::new();
                noise.hash_samples(&mut hasher);
                *word = word_from_digest(hasher);
            }
        });
    }

    pub(crate) fn fill_raw_samples(&self, data: &mut [u32]) {
        if data.is_empty() {
            return;
        }

        self.with_adc(|noise| {
            for sample in data {
                *sample = noise.raw_sample() as u32;
            }
        });
    }

    /// Run `f` with the ADC powered up, putting it back to sleep afterwards.
    /// Sampling outside a session returns whatever the powered-down ADC holds.
    pub(crate) fn with_adc<R>(&self, f: impl FnOnce(&Self) -> R) -> R {
        self.enable_adc();
        let result = f(self);
        self.adc.sleep();
        result
    }

    /// Absorb one output word's worth of raw ADC noise samples into `hasher`.
    ///
    /// Callers must start a fresh hasher per output word: carrying state across
    /// words would keep producing pseudorandom-looking output if the ADC became
    /// stuck, masking that failure until online health checks are available.
    pub(crate) fn hash_samples(&self, hasher: &mut Sha256) {
        for _ in 0..RAW_SAMPLES_PER_WORD {
            hasher.update(self.raw_sample().to_le_bytes());
        }
    }

    fn enable_adc(&self) {
        self.adc.reset();
        self.adc.set_prescaler(ADC_CLOCK_PRESCALER);
        self.adc.set_startup_time(ADC_STARTUP_TIME);
        self.adc.enable_channel(NOISE_CHANNEL);
    }

    fn raw_sample(&self) -> u16 {
        self.adc.start();
        self.adc.read(NOISE_CHANNEL)
    }
}

/// Finish `hasher` and take 32 bits of its digest as one output word.
pub(crate) fn word_from_digest(hasher: Sha256) -> u32 {
    let digest = hasher.finalize();
    u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]])
}
