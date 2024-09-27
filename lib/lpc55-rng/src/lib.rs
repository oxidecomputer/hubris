// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use core::{cmp, mem};
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use lpc55_pac::{PMC, RNG};
use rand_core::{impls, Error, RngCore};

/// The Lpc55Rng is a thin wrapper around the LPC55 hardware random number
/// generator (HRNG).
pub struct Lpc55Rng {
    pub pmc: PMC,
    pub rng: RNG,
}

impl Lpc55Rng {
    /// Create a new Lpc55Rng instance after powering on, enabling the clocks
    /// and reseting the underlying HRNG.
    pub fn new(pmc: PMC, rng: RNG, syscon: &Syscon) -> Self {
        pmc.pdruncfg0.modify(|_, w| w.pden_rng().poweredon());

        syscon.enable_clock(Peripheral::Rng);
        syscon.enter_reset(Peripheral::Rng);
        syscon.leave_reset(Peripheral::Rng);

        Lpc55Rng { pmc, rng }
    }
}

impl RngCore for Lpc55Rng {
    /// Get the next 4 bytes from the HRNG.
    fn next_u32(&mut self) -> u32 {
        impls::next_u32_via_fill(self)
    }

    /// Get the next 8 bytes from the HRNG.
    fn next_u64(&mut self) -> u64 {
        impls::next_u64_via_fill(self)
    }

    /// Fill the provided buffer with output from the HRNG.
    fn fill_bytes(&mut self, bytes: &mut [u8]) {
        self.try_fill_bytes(bytes).expect("fill_bytes")
    }

    /// Fill the provided buffer with output from the HRNG. If the HRNG
    /// can't service the request an error is returned.
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Error> {
        let mut filled = 0;
        while filled < dst.len() {
            // `new` takes ownership of the PMC & RNG before powering on the
            // RNG. If it gets turned off between then and now it's a bug.
            if self.pmc.pdruncfg0.read().pden_rng().bits() {
                panic!();
            }

            let src = self.rng.random_number.read().bits();
            let len = cmp::min(mem::size_of_val(&src), dst[filled..].len());

            dst[filled..filled + len]
                .copy_from_slice(&src.to_le_bytes()[..len]);
            filled += len;
        }

        Ok(())
    }
}
