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

        self.enable_adc();

        for word in data {
            *word = self.conditioned_word();
        }

        self.adc.sleep();
    }

    pub(crate) fn fill_raw_samples(&self, data: &mut [u32]) {
        if data.is_empty() {
            return;
        }

        self.enable_adc();

        for sample in data {
            *sample = self.raw_sample() as u32;
        }

        self.adc.sleep();
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

    /// Collect `RAW_SAMPLES_PER_WORD` raw ADC noise samples and extract 32 bits
    /// of conditioned output from their SHA-256 digest. The hasher is
    /// deliberately reinitialized for each word: carrying state across words
    /// would keep producing pseudorandom-looking output if the ADC became stuck,
    /// masking that failure until online health checks are available.
    fn conditioned_word(&self) -> u32 {
        let mut hasher = Sha256::new();
        for _ in 0..RAW_SAMPLES_PER_WORD {
            hasher.update(self.raw_sample().to_le_bytes());
        }
        let digest = hasher.finalize();
        u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]])
    }
}
