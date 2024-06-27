// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_lpc55_syscon_api::{Peripheral, Syscon};
use drv_rng_api::RngError;
use lpc55_pac::{pmc, rng, PMC, RNG};
use rand_core::{
    block::{BlockRng, BlockRngCore},
    Error, RngCore,
};

struct Lpc55Core {
    pmc: &'static pmc::RegisterBlock,
    rng: &'static rng::RegisterBlock,
    syscon: Syscon,
}

impl Lpc55Core {
    fn new(syscon: Syscon) -> Self {
        Lpc55Core {
            pmc: unsafe { &*PMC::ptr() },
            rng: unsafe { &*RNG::ptr() },
            syscon,
        }
    }
}

impl BlockRngCore for Lpc55Core {
    type Item = u32;
    type Results = [u32; 1];

    fn generate(&mut self, results: &mut Self::Results) {
        results[0] = self.rng.random_number.read().bits();
    }
}

pub struct Lpc55Rng(BlockRng<Lpc55Core>);

impl Lpc55Rng {
    pub fn new(syscon: Syscon) -> Self {
        Lpc55Rng(BlockRng::new(Lpc55Core::new(syscon)))
    }

    pub fn init(&self) {
        self.0
            .core
            .pmc
            .pdruncfg0
            .modify(|_, w| w.pden_rng().poweredon());

        self.0.core.syscon.enable_clock(Peripheral::Rng);

        self.0.core.syscon.enter_reset(Peripheral::Rng);
        self.0.core.syscon.leave_reset(Peripheral::Rng);
    }
}

impl RngCore for Lpc55Rng {
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }
    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }
    fn fill_bytes(&mut self, bytes: &mut [u8]) {
        self.0.fill_bytes(bytes)
    }
    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
        if self.0.core.pmc.pdruncfg0.read().pden_rng().bits() {
            return Err(RngError::PoweredOff.into());
        }

        self.0.try_fill_bytes(bytes)
    }
}
