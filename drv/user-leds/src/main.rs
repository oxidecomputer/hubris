// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for some basic dev board User LEDs.
//!
//! We assume that there are two user LEDs available, numbered 0 and 1. The
//! precise assignment of these to a particular dev board varies.
//!
//! # IPC protocol
//!
//! ## `led_on` (1)
//!
//! Turns an LED on by index.
//!
//! Request message format: single `u32` giving LED index.
//!
//! ## `led_off` (2)
//!
//! Turns an LED off by index.
//!
//! Request message format: single `u32` giving LED index.
//!
//! ## `led_toggle` (3)
//!
//! Toggles an LED by index.
//!
//! Request message format: single `u32` giving LED index.
//!
//! ## `led_blink` (4)
//!
//! Sets an LED to blink, specifying the LED by index
//!
//! Request message format: single `u32` giving LED index.

#![no_std]
#![no_main]

use core::marker::PhantomData;

use drv_user_leds_api::LedError;
use enum_map::EnumMap;
use idol_runtime::RequestError;
use userlib::{FromPrimitive, RecvMessage, set_timer_relative};

pub mod bsp;
use bsp::{Bsp, BspImpl};

pub type Led = <BspImpl as Bsp>::Led;
task_config::optional_task_config! {
    blink_at_start: &'static [Led],
}

const BLINK_INTERVAL: u32 = 500;

struct ServerImpl<B: Bsp> {
    blinking: EnumMap<B::Led, bool>,
    _bsp: PhantomData<B>,
}

impl<B: Bsp> idl::InOrderUserLedsImpl for ServerImpl<B> {
    fn led_on(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<LedError>> {
        let led = B::Led::from_usize(index).ok_or(LedError::NotPresent)?;
        self.blinking[led] = false;
        B::led_on(led);
        Ok(())
    }

    fn led_off(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<LedError>> {
        let led = B::Led::from_usize(index).ok_or(LedError::NotPresent)?;
        self.blinking[led] = false;
        B::led_off(led);
        Ok(())
    }

    fn led_toggle(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<LedError>> {
        let led = B::Led::from_usize(index).ok_or(LedError::NotPresent)?;
        self.blinking[led] = false;
        B::led_toggle(led);
        Ok(())
    }

    fn led_blink(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<LedError>> {
        let led = B::Led::from_usize(index).ok_or(LedError::NotPresent)?;
        let any_blinking = self.blinking.values().any(|b| *b);
        self.blinking[led] = true;

        if !any_blinking {
            set_timer_relative(BLINK_INTERVAL, notifications::TIMER_MASK);
        }
        Ok(())
    }
}

impl<B: Bsp> idol_runtime::NotificationHandler for ServerImpl<B> {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, bits: userlib::NotificationBits) {
        if bits.has_timer_fired(notifications::TIMER_MASK) {
            let mut any_blinking = false;
            for (led, blinking) in &self.blinking {
                if *blinking {
                    any_blinking = true;
                    B::led_toggle(led);
                }
            }
            if any_blinking {
                set_timer_relative(BLINK_INTERVAL, notifications::TIMER_MASK);
            }
        }
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    BspImpl::enable_led_pins();

    // Handle messages.
    let mut incoming = [0u8; idl::INCOMING_SIZE];
    let mut blinking: EnumMap<<BspImpl as Bsp>::Led, bool> = Default::default();
    if let Some(config) = TASK_CONFIG {
        for &led in config.blink_at_start {
            blinking[led] = true;
        }
        if !config.blink_at_start.is_empty() {
            set_timer_relative(BLINK_INTERVAL, notifications::TIMER_MASK);
        }
    }
    let mut server = ServerImpl {
        blinking,
        _bsp: PhantomData::<BspImpl>,
    };
    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

mod idl {
    use super::LedError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
