// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the LPC55 random number generator.
//!
//! Use the rng-api crate to interact with this driver.

#![no_std]
#![no_main]

use core::mem::size_of;
use drv_lpc55_syscon_api::Syscon;
use drv_rng_api::RngError;
use idol_runtime::{ClientError, NotificationHandler, RequestError};
use lib_lpc55_rng::Lpc55Rng;
use rand_chacha::ChaCha20Rng;
use rand_core::{impls, Error, RngCore, SeedableRng};
use userlib::task_slot;

task_slot!(SYSCON, syscon_driver);

// low-budget rand::rngs::adapter::ReseedingRng w/o fork stuff
struct ReseedingRng<T: SeedableRng> {
    inner: T,
    reseeder: Lpc55Rng,
    threshold: usize,
    bytes_until_reseed: usize,
}

impl<T> ReseedingRng<T>
where
    T: SeedableRng,
{
    fn new(mut reseeder: Lpc55Rng, threshold: usize) -> Result<Self, Error> {
        use ::core::usize::MAX;

        let threshold = if threshold == 0 { MAX } else { threshold };

        // try_trait_v2 is still experimental
        let inner = match T::from_rng(&mut reseeder) {
            Ok(rng) => rng,
            Err(err) => return Err(err),
        };
        Ok(ReseedingRng {
            inner,
            reseeder,
            threshold,
            bytes_until_reseed: threshold,
        })
    }
}

impl<T> RngCore for ReseedingRng<T>
where
    T: SeedableRng + RngCore,
{
    fn next_u32(&mut self) -> u32 {
        impls::next_u32_via_fill(self)
    }
    fn next_u64(&mut self) -> u64 {
        impls::next_u64_via_fill(self)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.try_fill_bytes(dest)
            .expect("Failed to get entropy from RNG.")
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        let num_bytes = dest.len();
        if num_bytes >= self.bytes_until_reseed || num_bytes >= self.threshold {
            // try_trait_v2 is still experimental
            self.inner = match T::from_rng(&mut self.reseeder) {
                Ok(rng) => rng,
                Err(e) => return Err(e),
            };
            self.bytes_until_reseed = self.threshold;
        } else {
            self.bytes_until_reseed -= num_bytes;
        }
        self.inner.try_fill_bytes(dest)
    }
}

struct Lpc55RngServer(ReseedingRng<ChaCha20Rng>);

impl Lpc55RngServer {
    fn new(reseeder: Lpc55Rng, threshold: usize) -> Result<Self, Error> {
        Ok(Lpc55RngServer(ReseedingRng::new(reseeder, threshold)?))
    }
}

impl idl::InOrderRngImpl for Lpc55RngServer {
    fn fill(
        &mut self,
        _: &userlib::RecvMessage,
        dest: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<RngError>> {
        let mut cnt = 0;
        const STEP: usize = size_of::<u32>();
        let mut buf = [0u8; STEP];
        // fill in multiples of STEP / RNG register size
        for _ in 0..(dest.len() / STEP) {
            self.0.try_fill_bytes(&mut buf).map_err(RngError::from)?;
            dest.write_range(cnt..cnt + STEP, &buf)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            cnt += STEP;
        }
        // fill in remaining
        let remain = dest.len() - cnt;
        assert!(remain < STEP);
        if remain > 0 {
            self.0.try_fill_bytes(&mut buf).map_err(RngError::from)?;
            dest.write_range(dest.len() - remain..dest.len(), &buf)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            cnt += remain;
        }
        Ok(cnt)
    }
}

impl NotificationHandler for Lpc55RngServer {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    let rng = Lpc55Rng::new(Syscon::from(SYSCON.get_task_id()));
    rng.init();

    let threshold = 0x100000; // 1 MiB
    let mut rng = Lpc55RngServer::new(rng, threshold)
        .expect("Failed to create Lpc55RngServer");
    let mut buffer = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut rng);
    }
}

mod idl {
    use drv_rng_api::RngError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
