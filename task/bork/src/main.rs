// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.

use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use stm32h7::stm32h753 as device;
use userlib::{set_timer_relative, task_slot, RecvMessage};

//const WATCHDOG_INTERVAL: u32 = 5000;

const TIMER_INTERVAL: u32 = 100;

task_slot!(SPROT, sprot);
task_slot!(SYS, sys);

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    LastId(u32),
    //Dogerr(drv_sprot_api::SprotError),
    None,
}
counted_ringbuf!(Trace, 8, Trace::None);

struct ServerImpl {
    //sprot: drv_sprot_api::SpRot,
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if (bits & notifications::TIMER_MASK) == 0 {
            return;
        }
    }
}

impl idl::InOrderBorkImpl for ServerImpl {
    fn ping(
        &mut self,
        _mgs: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        Ok(())
    }

    #[allow(unreachable_code)]
    fn wave_bye_bye(
        &mut self,
        _mgs: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        loop {
            cortex_m::asm::nop();
        }
        Ok(())
    }
}

#[export_name = "main"]
fn main() -> ! {
    //let sys = sys_api::Sys::from(SYS.get_task_id());
    //sys.enable_clock(sys_api::Peripheral::RtcApb);
    //sys.leave_reset(sys_api::Peripheral::RtcApb);

    let rtc = unsafe { &*device::RTC::ptr() };

    //rtc.wpr.write(|w| w.key().bits(0xca));
    //rtc.wpr.write(|w| w.key().bits(0x53));

    //rtc.tampcr.modify(|_, w| w.tamp3noerase().set_bit().tamp2noerase().set_bit().tamp1noerase().set_bit());

    let last = rtc.bkpr[1].read().bkp().bits();
    ringbuf_entry!(Trace::LastId(last));

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        //sprot: drv_sprot_api::SpRot::from(SPROT.get_task_id()),
    };
    set_timer_relative(TIMER_INTERVAL, notifications::TIMER_MASK);
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
