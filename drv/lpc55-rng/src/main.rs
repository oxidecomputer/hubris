// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the LPC55 random number generator.
//!
//! Use the rng-api crate to interact with this driver.

#![no_std]
#![no_main]

use core::{cmp, usize};
use drv_lpc55_syscon_api::Syscon;
use drv_rng_api::RngError;
use idol_runtime::{ClientError, NotificationHandler, RequestError};
use lib_lpc55_rng::Lpc55Rng;
use rand_chacha::ChaCha20Rng;
use rand_core::{impls, Error, RngCore, SeedableRng};
use sha3::{
    digest::crypto_common::{generic_array::GenericArray, OutputSizeUser},
    digest::FixedOutputReset,
    Digest, Sha3_256,
};
use userlib::task_slot;
use zeroize::Zeroizing;

task_slot!(SYSCON, syscon_driver);

// low-budget rand::rngs::adapter::ReseedingRng w/o fork stuff
struct ReseedingRng<T: SeedableRng, R: RngCore, H: Digest> {
    inner: T,
    reseeder: R,
    threshold: usize,
    bytes_until_reseed: usize,
    mixer: H,
}

impl<T, R, H> ReseedingRng<T, R, H>
where
    T: SeedableRng<Seed = [u8; 32]> + RngCore,
    R: RngCore,
    H: FixedOutputReset + Default + Digest,
    [u8; 32]: From<GenericArray<u8, <H as OutputSizeUser>::OutputSize>>,
{
    fn new(mut reseeder: R, threshold: usize) -> Result<Self, Error> {
        let threshold = if threshold == 0 {
            usize::MAX
        } else {
            threshold
        };

        Ok(ReseedingRng {
            inner: T::from_rng(&mut reseeder)?,
            reseeder,
            threshold,
            bytes_until_reseed: threshold,
            mixer: H::default(),
        })
    }
}

impl<T, R, H> RngCore for ReseedingRng<T, R, H>
where
    T: SeedableRng<Seed = [u8; 32]> + RngCore,
    R: RngCore,
    H: FixedOutputReset + Default + Digest,
    [u8; 32]: From<GenericArray<u8, <H as OutputSizeUser>::OutputSize>>,
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
        let mut filled = 0;

        while filled < dest.len() {
            if self.bytes_until_reseed > 0 {
                // fill dest as much as we can
                let len =
                    cmp::min(dest.len() - filled, self.bytes_until_reseed);
                self.inner.try_fill_bytes(&mut dest[filled..filled + len])?;

                filled += len;
                self.bytes_until_reseed -= len;
            } else {
                // create seed for next PRNG & reset mixer
                let mut buf = Zeroizing::new(T::Seed::default());

                // mix 32 bytes from current PRNG instance
                self.inner.try_fill_bytes(buf.as_mut())?;
                Digest::update(&mut self.mixer, buf.as_mut());

                // w/ 32 bytes from HRNG
                self.reseeder.try_fill_bytes(buf.as_mut())?;
                Digest::update(&mut self.mixer, buf.as_mut());

                // seed new RNG instance & reset mixer
                self.inner =
                    T::from_seed(self.mixer.finalize_fixed_reset().into());

                // reset reseed countdown
                self.bytes_until_reseed = self.threshold;
            }
        }

        Ok(())
    }
}

struct Lpc55RngServer(ReseedingRng<ChaCha20Rng, Lpc55Rng, Sha3_256>);

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
        let mut buf = [0u8; 32];
        while cnt < dest.len() {
            let len = cmp::min(buf.len(), dest.len() - cnt);

            self.0
                .try_fill_bytes(&mut buf[..len])
                .map_err(RngError::from)?;
            dest.write_range(cnt..cnt + len, &buf[..len])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

            cnt += len;
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
    let rng = Lpc55Rng::new(&Syscon::from(SYSCON.get_task_id()));

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
