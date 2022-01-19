// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the LPC55 random number generator.
//!
//! Use the rng-api crate to interact with this driver.

#![no_std]
#![no_main]

use core::mem::size_of;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use drv_rng_api::RngError;
use idol_runtime::{ClientError, RequestError};
use rand_core::block::{BlockRng, BlockRngCore};
use rand_core::RngCore;
use userlib::*;

use lpc55_pac as device;

task_slot!(SYSCON, syscon_driver);

struct Lpc55BlockRngCore {
    rng: &'static lpc55_pac::rng::RegisterBlock,
    pmc: &'static lpc55_pac::pmc::RegisterBlock,
}

impl Lpc55BlockRngCore {
    fn new() -> Self {
        Lpc55BlockRngCore {
            rng: unsafe { &*device::RNG::ptr() },
            pmc: unsafe { &*device::PMC::ptr() },
        }
    }
}

impl BlockRngCore for Lpc55BlockRngCore {
    type Item = u32;
    type Results = [u32; 1];

    fn generate(&mut self, results: &mut Self::Results) {
        results[0] = self.rng.random_number.read().bits();
    }
}

type Lpc55BlockRng = BlockRng<Lpc55BlockRngCore>;

impl idl::InOrderRngImpl for Lpc55BlockRng {
    fn fill(
        &mut self,
        _: &userlib::RecvMessage,
        dest: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<RngError>> {
        // if the oscilator is powered off, we won't get good RNG.
        if self.core.pmc.pdruncfg0.read().pden_rng().is_poweredoff() {
            return Err(RequestError::Runtime(RngError::PoweredOff));
        }

        let mut cnt = 0;
        const STEP: usize = size_of::<u32>();
        // fill in multiples of STEP / RNG register size
        for _ in 0..(dest.len() / STEP) {
            let number = self.next_u32();
            dest.write_range(cnt..cnt + STEP, &number.to_ne_bytes()[0..STEP])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            cnt += STEP;
        }
        // fill in remaining
        let remain = dest.len() - cnt;
        if remain > STEP {
            panic!("RNG state machine bork");
        }
        if remain > 0 {
            let ent = self.next_u32().to_ne_bytes();
            dest.write_range(dest.len() - remain..dest.len(), &ent[..remain])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            cnt += remain;
        }
        Ok(cnt)
    }
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = SYSCON.get_task_id();
    let syscon = Syscon::from(syscon);

    syscon.enable_clock(Peripheral::Rng);

    let rng = Lpc55BlockRngCore::new();
    let mut rng = Lpc55BlockRng::new(rng);

    let mut buffer = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut rng);
    }
}

mod idl {
    use drv_rng_api::RngError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
