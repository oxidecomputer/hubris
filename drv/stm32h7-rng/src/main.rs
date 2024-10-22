// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the STM32H7 random number generator.
//!
//! Use the rng-api crate to interact with this driver.

#![no_std]
#![no_main]

use drv_rng_api::RngError;
use drv_stm32xx_sys_api::{Peripheral, Sys};
use idol_runtime::{ClientError, NotificationHandler, RequestError};

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use userlib::{hl, task_slot, RecvMessage};

task_slot!(SYS, sys);

struct Stm32h7Rng {
    cr: &'static device::rng::CR,
    dr: &'static device::rng::DR,
    sr: &'static device::rng::SR,
    sys: Sys,
}

impl Stm32h7Rng {
    fn new() -> Self {
        let registers = unsafe { &*device::RNG::ptr() };
        Stm32h7Rng {
            cr: &registers.cr,
            dr: &registers.dr,
            sr: &registers.sr,
            sys: Sys::from(SYS.get_task_id()),
        }
    }

    fn init(&mut self) -> Result<(), RngError> {
        self.sys.enable_clock(Peripheral::Rng);
        self.enable_rng();
        if self.is_clock_error() {
            let err = RngError::ClockError;
            return Err(err);
        }

        Ok(())
    }

    fn read(&mut self) -> Result<u32, RngError> {
        let mut retries = 10;
        while !self.is_data_ready() && retries > 0 {
            hl::sleep_for(1);
            retries -= 1;
        }
        if !self.is_data_ready() {
            return Err(RngError::NoData);
        }
        if self.is_seed_error() {
            return Err(RngError::SeedError);
        }
        Ok(self.dr.read().rndata().bits())
    }

    fn enable_rng(&self) {
        self.cr.modify(|_, w| w.rngen().set_bit());
    }

    fn is_clock_error_detect(&self) -> bool {
        self.cr.read().ced().bits()
    }

    fn is_clock_error(&self) -> bool {
        // if clock error detection is disabled, CECS & CEIS won't be valid
        if self.is_clock_error_detect() {
            return self.sr.read().cecs().bits()
                || self.sr.read().ceis().bits();
        }
        false
    }

    fn is_data_ready(&self) -> bool {
        self.sr.read().drdy().bits()
    }

    fn is_seed_error(&self) -> bool {
        self.sr.read().secs().bits() || self.sr.read().seis().bits()
    }
}

struct Stm32h7RngServer {
    rng: Stm32h7Rng,
}

impl Stm32h7RngServer {
    fn new(rng: Stm32h7Rng) -> Self {
        Stm32h7RngServer { rng }
    }
}

impl idl::InOrderRngImpl for Stm32h7RngServer {
    fn fill(
        &mut self,
        _: &RecvMessage,
        dest: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<RngError>> {
        let mut cnt = 0;
        for _ in 0..(dest.len() / 4) {
            let ent = self.rng.read()?;
            dest.write_range(cnt..cnt + 4, &ent.to_ne_bytes()[0..4])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            cnt += 4;
        }

        let remain = dest.len() - cnt;
        if remain > 4 {
            panic!("RNG state machine bork");
        }
        if remain > 0 {
            let ent = self.rng.read()?;
            dest.write_range(cnt..dest.len(), &ent.to_ne_bytes()[0..remain])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            cnt += remain;
        }

        Ok(cnt)
    }
}

impl NotificationHandler for Stm32h7RngServer {
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
    let mut rng = Stm32h7Rng::new();
    rng.init().expect("init failed");

    let mut srv = Stm32h7RngServer::new(rng);
    let mut buffer = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut srv);
    }
}

mod idl {
    use drv_rng_api::RngError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
