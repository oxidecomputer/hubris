// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//!
//! An exceedingly simple driver to deal with the management of Meanwell power
//! supplies from a Gimletlet on a benchtop.  This driver has nothing
//! Meanwell-specific (or indeed even power-specific); this is really just a
//! wrapper around the seven GPIOs present on J16 on the Gimletlet.  We also
//! use this opportunity to indicate a different LED pattern (a quick blue LED
//! blink, once per second) to make it clear from across a room that a
//! particularly Gimletlet is serving to manage Meanwell supplies.
//!
#![no_std]
#![no_main]

use drv_meanwell_api::MeanwellError;
use idol_runtime::NotificationHandler;
use idol_runtime::RequestError;
use userlib::*;

use drv_stm32xx_sys_api as sys_api;

struct ServerImpl {
    deadline: u64,
    led_on: bool,
}

cfg_if::cfg_if! {
    if #[cfg(any(target_board = "gimletlet-1", target_board = "gimletlet-2"))] {
        const MEANWELL_PINS: &[sys_api::PinSet] = &[
            sys_api::Port::B.pin(14),
            sys_api::Port::B.pin(15),
            sys_api::Port::D.pin(8),
            sys_api::Port::D.pin(9),
            sys_api::Port::D.pin(10),
            sys_api::Port::D.pin(11),
            sys_api::Port::D.pin(12),
        ];

        // In keeping with the Meanwell's blue indicator LED, we use only the
        // blue LED to indicate that this is a Meanwell management Gimletlet.
        const LED_INDEX: usize = 3;
    } else {
        compile_error!("unsupported Meanwell board");
    }
}

fn set(index: usize, val: bool) -> Result<(), RequestError<MeanwellError>> {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    if index >= MEANWELL_PINS.len() {
        Err(MeanwellError::NotPresent.into())
    } else {
        sys.gpio_set_to(MEANWELL_PINS[index], val);
        Ok(())
    }
}

fn get(index: usize) -> Result<bool, RequestError<MeanwellError>> {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    if index >= MEANWELL_PINS.len() {
        Err(MeanwellError::NotPresent.into())
    } else {
        let val = sys.gpio_read(MEANWELL_PINS[index]);
        Ok(val != 0)
    }
}

impl idl::InOrderMeanwellImpl for ServerImpl {
    fn power_on(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<MeanwellError>> {
        set(index, true)
    }

    fn power_off(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<MeanwellError>> {
        set(index, false)
    }

    fn is_on(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<bool, RequestError<MeanwellError>> {
        get(index)
    }
}

task_slot!(USER_LEDS, user_leds);
task_slot!(SYS, sys);

const TIMER_INTERVAL_LONG: u32 = 900;
const TIMER_INTERVAL_SHORT: u32 = 100;

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        if userlib::sys_get_timer().deadline.is_some() {
            return;
        }

        let user_leds =
            drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());

        let interval = if !self.led_on {
            user_leds.led_on(LED_INDEX).unwrap();
            self.led_on = true;
            TIMER_INTERVAL_SHORT
        } else {
            user_leds.led_off(LED_INDEX).unwrap();
            self.led_on = false;
            TIMER_INTERVAL_LONG
        };

        // This is technically slightly wrong in that, if there's CPU
        // contention, the LEDs may blink at slightly lower than their intended
        // frequency. But since the frequency isn't load-bearing, this is
        // significantly less code:
        self.deadline = set_timer_relative(interval, notifications::TIMER_MASK);
    }
}

#[export_name = "main"]
fn main() -> ! {
    let deadline = sys_get_timer().now;

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    let mut serverimpl = ServerImpl {
        led_on: false,
        deadline,
    };

    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    //
    // We can safely do this even if the pins are already configured without
    // changing their state.
    //
    for pin in MEANWELL_PINS {
        sys.gpio_configure_output(
            *pin,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        )
    }

    let mut incoming = [0u8; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut incoming, &mut serverimpl);
    }
}

mod idl {
    use super::MeanwellError;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
