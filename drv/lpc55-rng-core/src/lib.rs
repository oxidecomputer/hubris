#![no_std]

use chacha20::ChaCha20RngCore;
use core::convert::TryInto;
use core::mem::size_of;
use lpc55_pac as device;
use rand_core::{block::BlockRngCore, impls, Error, RngCore, SeedableRng};

pub struct Rng {
    reg: &'static device::rng::RegisterBlock,
}

impl From<&'static device::rng::RegisterBlock> for Rng {
    fn from(reg: &'static device::rng::RegisterBlock) -> Self {
        Self { reg }
    }
}

impl Rng {
    pub fn initialize(&mut self) {
        // This initialization sequence is taken from 48.14.5 of the
        // July 2020 user manual. It is assumed that all clocks have
        // been turned on.
        //
        // This also only applies to the 1B version of the device
        loop {
            const REF_CHI_SQUARED: u8 = 0x2;

            self.reg
                .online_test_cfg
                .modify(|_, w| unsafe { w.data_sel().bits(4) });
            self.reg
                .online_test_cfg
                .modify(|_, w| w.activate().set_bit());

            while self.reg.online_test_val.read().min_chi_squared().bits()
                > self.reg.online_test_val.read().max_chi_squared().bits()
            {
            }

            if self.reg.online_test_val.read().max_chi_squared().bits()
                > REF_CHI_SQUARED
            {
                self.reg
                    .online_test_cfg
                    .modify(|_, w| w.activate().clear_bit());
                self.reg.counter_cfg.modify(|r, w| unsafe {
                    w.shift4x().bits(r.shift4x().bits() + 1)
                });
            } else {
                break;
            }
        }
    }

    pub fn get_random(&mut self) -> Option<u32> {
        let mut cnt = 0;
        while self.reg.counter_val.read().refresh_cnt().bits() < 32 {
            if cnt > 10000 {
                return None;
            } else {
                cnt = cnt + 1;
            }
        }

        let number = self.reg.random_number.read().bits();

        Some(number)
    }
}

// This is intentionally _not_ marked as CryptoRng!
// The hardware has not passed certification!
impl RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        self.get_random().unwrap()
    }

    fn next_u64(&mut self) -> u64 {
        impls::next_u64_via_u32(self)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        impls::fill_bytes_via_next(self, dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        Ok(self.fill_bytes(dest))
    }
}

pub struct ReseedingCore {
    inner: ChaCha20RngCore,
    reseeder: Rng,
    threshold: u32,
    bytes_until_reseed: u32,
}

impl BlockRngCore for ReseedingCore {
    type Item = <ChaCha20RngCore as BlockRngCore>::Item;
    type Results = <ChaCha20RngCore as BlockRngCore>::Results;

    fn generate(&mut self, results: &mut Self::Results) {
        let num_bytes: u32 = (results.as_ref().len() * size_of::<Self::Item>())
            .try_into()
            .unwrap();

        if self.bytes_until_reseed <= num_bytes {
            if let Err(_) = self.reseed() {
                panic!("Reseeding RNG failed");
            }
        }
        self.bytes_until_reseed -= num_bytes;
        self.inner.generate(results);
    }
}

impl ReseedingCore {
    pub fn new(threshold: u32, mut reseeder: Rng) -> Self {
        use ::core::u32::MAX;

        let threshold = if threshold == 0 { MAX } else { threshold };

        let rng = ChaCha20RngCore::from_rng(&mut reseeder).unwrap();

        ReseedingCore {
            inner: rng,
            reseeder,
            threshold: threshold,
            bytes_until_reseed: threshold,
        }
    }

    fn reseed(&mut self) -> Result<(), Error> {
        ChaCha20RngCore::from_rng(&mut self.reseeder).map(|result| {
            self.bytes_until_reseed = self.threshold;
            self.inner = result
        })
    }
}
