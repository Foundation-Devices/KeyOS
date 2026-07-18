use rand_chacha::rand_core::RngCore;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha8Rng;

pub struct Trng {
    rng: ChaCha8Rng,
    msgcount: u16, // re-print the message every time we rollover
}

impl Trng {
    pub fn new() -> Trng {
        Trng {
            rng: ChaCha8Rng::seed_from_u64(xous::TESTING_RNG_SEED.load(core::sync::atomic::Ordering::SeqCst)),
            msgcount: 0,
        }
    }

    pub fn fill_buf(&mut self, data: &mut [u32], _source: trng::TrngSource) {
        if self.msgcount < 3 {
            log::info!("hosted mode TRNG is *not* random, it is a deterministic PRNG");
        }
        self.msgcount += 1;
        for d in data {
            *d = self.rng.next_u32();
        }
    }

    pub fn fill_avalanche_raw_samples(&mut self, data: &mut [u32]) {
        if self.msgcount < 3 {
            log::info!("hosted mode TRNG is *not* random, it is a deterministic PRNG");
        }
        self.msgcount += 1;
        for d in data {
            *d = self.rng.next_u32() & 0x0fff;
        }
    }
}
