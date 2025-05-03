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

use ringbuf::{counted_ringbuf, ringbuf_entry};

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use userlib::*;

task_slot!(SYS, sys);

counted_ringbuf!(Trace, 32, Trace::Blank);

#[derive(Copy, Clone, Debug, Eq, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    Blank,
    ClockError,
    SeedError,
    Recovered,
}

struct Stm32h7Rng {
    regs: &'static device::rng::RegisterBlock,
    sys: Sys,
}

impl Stm32h7Rng {
    fn new() -> Self {
        let registers = unsafe { &*device::RNG::ptr() };
        Stm32h7Rng {
            regs: registers,
            sys: Sys::from(SYS.get_task_id()),
        }
    }

    fn init(&mut self) {
        // Reset the RNG, so that if we restart, so does the hardware.
        self.sys.enter_reset(Peripheral::Rng);
        self.sys.leave_reset(Peripheral::Rng);

        // Turn on the RNG's clock and bus connection.
        self.sys.enable_clock(Peripheral::Rng);

        // Turn it on. This _starts_ the initialization process, which takes
        // time on the order of microseconds. The process ends with setting
        // either the DRDY bit or an ERROR bit in SR, which we'll notice when we
        // attempt to pull data out of it.
        self.regs.cr.modify(|_, w| w.rngen().set_bit());
    }

    fn read(&mut self) -> Result<u32, RngError> {
        for _retry in 0..10 {
            // Sample our status register.
            let sr = self.regs.sr.read();

            // We do not expect clock errors. The clock configuration is static,
            // and clock errors can only occur if the RNG kernel clock is
            // waaaaay below the AHB clock, which we don't do. But, we're
            // interested in finding out they _were happening_ if they happen,
            // so:
            if sr.ceis().bit_is_set() {
                // TODO this should be an ereport
                ringbuf_entry!(Trace::ClockError);
                // Clear it so we don't repeat ourselves. The two writable bits
                // in this register are write-one-to-preserve, so weirdly we
                // want to write SEIS to 1 here.
                self.regs.sr.modify(|_, w| {
                    w.seis().set_bit();
                    w.ceis().clear_bit();
                    w
                });
            }

            // If an error occurred, return it so that our caller can attempt
            // recovery.
            if sr.seis().bit_is_set() {
                // TODO this should be an ereport
                ringbuf_entry!(Trace::SeedError);
                // Clear it so we don't repeat ourselves. The two writable bits
                // in this register are write-one-to-preserve, so weirdly we
                // want to write CEIS to 1 here.
                self.regs.sr.modify(|_, w| {
                    w.ceis().set_bit();
                    w.seis().clear_bit();
                    w
                });
                return Err(RngError::SeedError);
            }
            // If data is ready, we can yield it.
            if sr.drdy().bit_is_set() {
                let data = self.regs.dr.read().rndata().bits();
                // There's a note in section 34.3.5 of the reference manual that
                // reads:
                //
                // > When data is not ready (DRDY = 0) RNG_DR returns zero. It
                // > is recommended to always verify that RNG_DR is different
                // > from zero. Because when it is the case a seed error
                // > occurred between RNG_SR polling and RND_DR output reading
                // > (rare event).
                //
                // Why it's not sufficient to check SR.SEIS after reading DR, I
                // couldn't tell you. But we'll implement their weird
                // recommendation.
                if data != 0 {
                    return Ok(data);
                }
            }

            // Otherwise, keep trying after waiting a tick.
            hl::sleep_for(1);
        }

        Err(RngError::NoData)
    }

    fn attempt_recovery(&mut self) -> Result<(), RngError> {
        // The recovery procedure is in section 34.3.7 of the manual.
        //
        // Step one is to clear the SEIS flag. We expect the SEIS flag to have
        // already been cleared by `read`.

        // Next, we read 12 words to clear the pipeline. The number of words is
        // 12. Not 11, not 13. 12 is the number given in the manual with no
        // explanation.
        for _ in 0..12 {
            let _ = self.regs.dr.read();
        }

        // Finally, we check whether SEIS is clear.
        if self.regs.sr.read().seis().bit_is_clear() {
            // Yay!
            ringbuf_entry!(Trace::Recovered);
            Ok(())
        } else {
            Err(RngError::SeedError)
        }
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
        let len = dest.len();
        let mut cnt = 0;

        // Keep track of whether we're making progress for recovery purposes. We
        // start this at 'true' so that, if we detect a seed error at the start
        // of this function, we'll try to recover at least once.
        let mut made_progress = true;

        while cnt < len {
            match self.rng.read() {
                Ok(word) => {
                    // We have 32 shiny bits of data. Prepare to write that
                    // much, or less if we're at the tail, to the client.
                    let word_bytes = &word.to_ne_bytes();
                    let chunk_size = usize::min(len - cnt, 4);
                    dest.write_range(
                        cnt..cnt + chunk_size,
                        &word_bytes[..chunk_size],
                    )
                    .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
                    cnt += chunk_size;

                    // Note that we successfully did a thing, for recovery
                    // purposes.
                    made_progress = true;
                }
                Err(RngError::SeedError) if made_progress => {
                    // It may be worth attempting recovery. Recovery may not
                    // succeed, in which case this'll just bomb right out of
                    // here and send an error to the client.
                    self.rng.attempt_recovery()?;

                    // Clear the progress flag so that if another seed error
                    // happens immediately, we'll exit.
                    made_progress = false;
                }
                Err(e) => {
                    // We have no way to recover from the other cases.
                    return Err(e.into());
                }
            }
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
    rng.init();

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
