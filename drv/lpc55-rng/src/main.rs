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
use idol_runtime::{ClientError, NotificationHandler, RequestError};
use rand_chacha::ChaCha20Rng;
use rand_core::block::{BlockRng, Generator};
use rand_core::{Rng, SeedableRng, TryRng};
use userlib::task_slot;

use lpc55_pac as device;

task_slot!(SYSCON, syscon_driver);

struct Lpc55Core {
    pmc: &'static lpc55_pac::pmc::RegisterBlock,
    rng: &'static lpc55_pac::rng::RegisterBlock,
    syscon: Syscon,
}

impl Lpc55Core {
    fn new() -> Self {
        let syscon = SYSCON.get_task_id();
        Lpc55Core {
            pmc: unsafe { &*device::PMC::ptr() },
            rng: unsafe { &*device::RNG::ptr() },
            syscon: Syscon::from(syscon),
        }
    }
}

impl Generator for Lpc55Core {
    type Output = [u32; 1];

    fn generate(&mut self, output: &mut Self::Output) {
        output[0] = self.rng.random_number.read().bits();
    }
}

struct Lpc55Rng(BlockRng<Lpc55Core>);

impl Lpc55Rng {
    fn new() -> Self {
        Lpc55Rng(BlockRng::new(Lpc55Core::new()))
    }

    fn init(&self) {
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

impl TryRng for Lpc55Rng {
    type Error = RngError;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(self.0.next_word())
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(self.0.next_u64_from_u32())
    }
    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error> {
        if self.0.core.pmc.pdruncfg0.read().pden_rng().bits() {
            return Err(RngError::PoweredOff);
        }
        self.0.fill_bytes(bytes);
        Ok(())
    }
}

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
    fn new(mut reseeder: Lpc55Rng, threshold: usize) -> Result<Self, RngError> {
        let threshold = if threshold == 0 {
            usize::MAX
        } else {
            threshold
        };

        let inner = T::try_from_rng(&mut reseeder)?;
        Ok(ReseedingRng {
            inner,
            reseeder,
            threshold,
            bytes_until_reseed: threshold,
        })
    }
}

impl<T> TryRng for ReseedingRng<T>
where
    T: SeedableRng + Rng,
{
    type Error = RngError;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bs = [0u8; 4];
        self.try_fill_bytes(&mut bs)?;
        Ok(u32::from_le_bytes(bs))
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bs = [0u8; 8];
        self.try_fill_bytes(&mut bs)?;
        Ok(u64::from_le_bytes(bs))
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Self::Error> {
        let num_bytes = dest.len();
        if num_bytes >= self.bytes_until_reseed || num_bytes >= self.threshold {
            self.inner = T::try_from_rng(&mut self.reseeder)?;
            self.bytes_until_reseed = self.threshold;
        } else {
            self.bytes_until_reseed -= num_bytes;
        }
        self.inner.fill_bytes(dest);
        Ok(())
    }
}

struct Lpc55RngServer(ReseedingRng<ChaCha20Rng>);

impl Lpc55RngServer {
    fn new(reseeder: Lpc55Rng, threshold: usize) -> Result<Self, RngError> {
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
            self.0.try_fill_bytes(&mut buf)?;
            dest.write_range(cnt..cnt + STEP, &buf)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            cnt += STEP;
        }
        // fill in remaining
        let remain = dest.len() - cnt;
        assert!(remain < STEP);
        if remain > 0 {
            self.0.try_fill_bytes(&mut buf)?;
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

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    let rng = Lpc55Rng::new();
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
