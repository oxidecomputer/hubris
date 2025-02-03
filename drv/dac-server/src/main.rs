// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32H7 HASH server.
//!
//! This server is responsible for managing access to the HASH engine.
//!

#![no_std]
#![no_main]

// use core::convert::TryInto;
use userlib::*;

use drv_stm32xx_sys_api as sys_api;

use idol_runtime::{
    ClientError, Leased, LenLimit, NotificationHandler, RequestError, R,
};

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_dac_api::DacError;
task_slot!(SYS, sys);

pub struct Dac {
    reg: &'static device::dac::RegisterBlock,
    gpioa: &'static device::gpioa::RegisterBlock,
    rcc: &'static device::rcc::RegisterBlock,
    tim6: &'static device::tim6::RegisterBlock,
}

impl Dac {
    pub fn new(
        reg: &'static device::dac::RegisterBlock,
        gpioa: &'static device::gpioa::RegisterBlock,
        rcc: &'static device::rcc::RegisterBlock,
        tim6: &'static device::tim6::RegisterBlock,
    ) -> Self {
        Self {
            reg,
            gpioa,
            rcc,
            tim6,
        }
    }

}

struct ServerImpl {
    dac: Dac,
    // block: [u8; 512],
}


// fn hash_hw_reset() {
//     let sys = sys_api::Sys::from(SYS.get_task_id());
//     sys.enter_reset(sys_api::Peripheral::Hash);
//     sys.disable_clock(sys_api::Peripheral::Hash);
//     sys.enable_clock(sys_api::Peripheral::Hash);
//     sys.leave_reset(sys_api::Peripheral::Hash);
// }

#[export_name = "main"]
fn main() -> ! {
    //hash_hw_reset();

    let reg = unsafe { &*device::DAC::ptr() };
    let gpioa = unsafe { &*device::GPIOA::ptr() };
    let rcc = unsafe { &*device::RCC::ptr() };
    let tim6 = unsafe { &*device::TIM6::ptr() };


    let dac = Dac::new(reg, gpioa, rcc, tim6);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        dac,
        // block: [0; 512],
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}


impl idl::InOrderDacImpl for ServerImpl {

    fn test_pattern(
        &mut self,
        msg: &RecvMessage,
    ) -> Result<(), RequestError<DacError>> {
        unsafe {
            // self.dac.reg.cr.modify(|_, w| {
            //     w.en1().set_bit()
            //     .ten1().set_bit()
            //     .wave1().bits(0b11)
            //     .en2().set_bit()
            //     .ten2().set_bit()
            //     .wave2().bits(0b11)
            // });

            self.dac.gpioa.moder.modify(|_, w| {
                w.moder4().alternate()
            });


            // self.dac.rcc.apb1lenr.modify(|_, w| {
            //     w.dac12en().set_bit()
            //     .tim6en().set_bit()
            // });

            self.dac.tim6.arr.write(|w| {
                w.bits(0xff)
            });

            self.dac.tim6.egr.write(|w| {
                w.ug().set_bit()
            });

            self.dac.tim6.cr2.modify(|_, w| {
                w.mms().update()
            });

            self.dac.tim6.cr1.modify(|_, w| {
                w.cen().set_bit()
            });


            self.dac.reg.cr.modify(|_, w| {
                w.mamp1().bits(0b1111)
            });
            self.dac.reg.cr.modify(|_, w| {
                w.wave1().bits(0b11)
            });

            // self.dac.reg.dhr12r1.write(|w|{
            //     w.bits(0b111111111111)
            // });
            // self.dac.reg.swtrgr.write(|w| {
            //     w.swtrig1().set_bit()
            // });
            self.dac.reg.cr.modify(|_, w| {
                w.tsel1().bits(6)
            });
            self.dac.reg.cr.modify(|_, w| {
                w.ten1().set_bit()
            });
            self.dac.reg.cr.modify(|_, w| {
                w.en1().set_bit()
            });

        }
        Ok(())
    }

    // fn init_sha256(
    //     &mut self,
    //     _: &RecvMessage,
    // ) -> Result<(), RequestError<HashError>> {
    //     hash_hw_reset();
    //     // TODO: Solve multiple clients needing
    //     // context storage for suspend/resume and/or mutual exclusion.
    //     self.hash.init_sha256()?;
    //     Ok(())
    // }

    // fn update(
    //     &mut self,
    //     _: &RecvMessage,
    //     len: u32,
    //     data: LenLimit<Leased<R, [u8]>, 512>,
    // ) -> Result<(), RequestError<HashError>> {
    //     if len == 0 || data.len() < len as usize {
    //         return Err(HashError::NoData.into());
    //     }
    //     data.read_range(0..len as usize, &mut self.block[..len as usize])
    //         .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
    //     self.hash.update(&self.block[..len as usize])?;
    //     Ok(())
    // }

    // fn finalize_sha256(
    //     &mut self,
    //     _: &RecvMessage,
    // ) -> Result<[u8; SHA256_SZ], RequestError<HashError>> {
    //     let mut sha256_sum = [0; SHA256_SZ];
    //     self.hash.finalize_sha256(&mut sha256_sum)?;
    //     Ok(sha256_sum)
    // }

    // fn digest_sha256(
    //     &mut self,
    //     _: &RecvMessage,
    //     len: u32,
    //     data: LenLimit<Leased<R, [u8]>, 512>,
    // ) -> Result<[u8; SHA256_SZ], RequestError<HashError>> {
    //     let mut sha256_sum = [0; SHA256_SZ];

    //     if len == 0 || data.len() < len as usize {
    //         return Err(HashError::NoData.into()); // TODO: not the right name
    //     }

    //     data.read_range(0..len as usize, &mut self.block[..len as usize])
    //         .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
    //     self.hash
    //         .digest_sha256(&self.block[..len as usize], &mut sha256_sum)?;
    //     Ok(sha256_sum)
    // }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

mod idl {
    //use drv_hash_api::HashError;
    use drv_dac_api::DacError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
