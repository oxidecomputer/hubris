#![no_std]

use rand_core::{impls, CryptoRng, Error, RngCore};
use userlib::*;
use zerocopy::AsBytes;

#[derive(AsBytes)]
#[repr(C)]
struct FetchRandomNumber;

impl hl::Call for FetchRandomNumber {
    const OP: u16 = 0;
    type Response = u32;
    type Err = u32;
}

#[derive(Copy, Clone, Debug)]
struct HubrisRng(TaskId);

impl From<TaskId> for HubrisRng {
    fn from(t: TaskId) -> Self {
        Self(t)
    }
}

// This represents the low level driver call. We could theoretically use this
// without the RNG API but wrapping it in this is easier for seeding.
impl RngCore for HubrisRng {
    fn next_u32(&mut self) -> u32 {
        hl::send(self.0, &FetchRandomNumber).expect("rng failed")
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

impl CryptoRng for HubrisRng {}
