// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Random Number Generation
//!
//! This task will produce random u32 values for you, if you ask nicely.
//!
//! An example:
//!
//! ```ignore
//! #[derive(AsBytes)]
//! #[repr(C)]
//! struct FetchRandomNumber;
//!
//! impl hl::Call for FetchRandomNumber {
//!     const OP: u16 = 0;
//!     type Response = u32;
//!     type Err = u32;
//! }
//!
//! let num = hl::send(rng, &FetchRandomNumber).expect("could not ask the rng for a number");
//!
//! hprintln!("got {} from the rng", num).ok();
//! ```

#![no_std]
#![no_main]

use drv_lpc55_syscon_api::{Peripheral, Syscon};
use userlib::*;
use zerocopy::AsBytes;

use lpc55_pac as device;

task_slot!(SYSCON, syscon_driver);

#[repr(u32)]
enum ResponseCode {
    BadArg = 1,
    PoweredOff = 2,
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = SYSCON.get_task_id();
    let syscon = Syscon::from(syscon);

    syscon.enable_clock(Peripheral::Rng);

    let rng = unsafe { &*device::RNG::ptr() };
    let pmc = unsafe { &*device::PMC::ptr() };

    let mut buffer = [0u32; 1];

    loop {
        hl::recv_without_notification(
            buffer.as_bytes_mut(),
            |_op: u16, msg| -> Result<(), ResponseCode> {
                let (_msg, caller) =
                    msg.fixed::<(), u32>().ok_or(ResponseCode::BadArg)?;

                // if the oscilator is powered off, we won't get good RNG.
                if pmc.pdruncfg0.read().pden_rng().is_poweredoff() {
                    return Err(ResponseCode::PoweredOff);
                }

                let number = rng.random_number.read().bits();

                caller.reply(number);

                Ok(())
            },
        );
    }
}
