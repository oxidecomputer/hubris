// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

#[cfg(feature = "custom-getrandom")]
use core::num::NonZeroU32;
#[cfg(feature = "custom-getrandom")]
use getrandom::Error;

use userlib::*;

#[repr(u32)]
#[derive(Copy, Clone, Debug, FromPrimitive)]
pub enum RngError {
    BadArg = 1,
    PoweredOff = 2,
}

impl From<RngError> for u32 {
    fn from(rc: RngError) -> Self {
        rc as u32
    }
}

#[cfg(feature = "custom-getrandom")]
task_slot!(RNG, rng_driver);

#[cfg(feature = "custom-getrandom")]
pub fn rng_getrandom(dest: &mut [u8]) -> Result<(), Error> {
    match rng_fill(RNG.get_task_id(), dest) {
        Ok(_) => Ok(()),
        Err(err) => {
            let rc = NonZeroU32::new(err as u32).unwrap();
            Err(Error::from(rc))
        }
    }
}

pub fn rng_fill(task: TaskId, buf: &mut [u8]) -> Result<usize, RngError> {
    let mut response = [0; 4];
    let (rc, len) = sys_send(task, 0, &[], &mut response, &[Lease::from(buf)]);
    if let Some(err) = RngError::from_u32(rc) {
        Err(err)
    } else {
        assert_eq!(len, 4);
        Ok(usize::from_ne_bytes(response))
    }
}
