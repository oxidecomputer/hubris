// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Root of trust for reporting (RoT-R) task.
//!
//! Use the attest-api crate to interact with this task.

#![no_std]
#![no_main]

use button_api::ButtonError;
use drv_lpc55_gpio_api::{PintCondition, PintOp, Value};
// use button_api::*;
use crate::idl::INCOMING_SIZE;
use idol_runtime::{
    // ClientError, Leased, LenLimit,
    NotificationHandler,
    RequestError,
    // R, W,
};
use userlib::{
    set_timer_relative, sys_irq_control, sys_set_timer, task_slot, TaskId,
    UnwrapLite,
};

// Time is in approximate ms
const ON_DELAY: u32 = 1 * 1000;
const OFF_DELAY: u32 = ON_DELAY / 2;
const QUICKPRESS: u32 = 1500;

task_slot!(GPIO, gpio_driver);

struct ButtonServer {
    gpio: drv_lpc55_gpio_api::Pins,
    last_button_press: u64,
    quick: usize,

    /// LED state
    on: bool,
    /// On ms or 0 to disable timer
    on_ms: u32,
    /// Off ms or 0 to disable timer
    off_ms: u32,
    rgb: u8,
}

impl ButtonServer {
    fn increment(&mut self) -> Result<u8, ButtonError> {
        self.rgb = (self.rgb + 1) % 8;
        let _ = self.update_leds()?;
        Ok(self.rgb)
    }

    fn to_rgb(v: u8) -> (bool, bool, bool) {
        ((v & 0b100) != 0, (v & 0b010) != 0, (v & 0b001) != 0)
    }

    fn update_leds(&self) -> Result<u8, ButtonError> {
        let leds = self.rgb;
        let (r, g, b) = if self.on {
            Self::to_rgb(leds)
        } else {
            (false, false, false)
        };
        // LED signals are active low.
        self.gpio
            .set_val(RED_LED, if r { Value::Zero } else { Value::One });
        self.gpio
            .set_val(GREEN_LED, if g { Value::Zero } else { Value::One });
        self.gpio
            .set_val(BLUE_LED, if b { Value::Zero } else { Value::One });
        Ok(leds)
    }

    fn timer_expiry(&mut self) {
        if self.on {
            // LEDs were on
            if self.off_ms > 0 {
                self.on = false;
                let _ = self.update_leds();
                set_timer_relative(self.off_ms, notifications::TIMER_MASK);
            } else {
                // no off timer; go to the next pattern and update.
                let _ = self.increment();
                set_timer_relative(self.on_ms, notifications::TIMER_MASK);
            }
        } else {
            // LEDs were off
            if self.on_ms > 0 {
                self.on = true;
                set_timer_relative(self.on_ms, notifications::TIMER_MASK);
            } else {
                // Leave them off and stop timer (this should be a redundant stop).
                sys_set_timer(None, notifications::TIMER_MASK);
            }
            let _ = self.update_leds();
        }
    }

    fn handle_button_press(&mut self) -> Result<u8, ButtonError> {
        let now = userlib::sys_get_timer().now;
        let last = self.last_button_press;
        self.last_button_press = now;
        let delta = now - last;
        if delta < QUICKPRESS as u64 {
            self.quick += 1;
            match self.quick {
                // Second press:  Stop timer and turn off LEDs
                1 => {
                    self.on = false;
                    self.on_ms = 0;
                    self.off_ms = 0;
                    sys_set_timer(None, notifications::TIMER_MASK);
                }
                // Third press:  Blink 1s on, 0.5s off.
                2 => {
                    self.on = true;
                    self.on_ms = ON_DELAY;
                    self.off_ms = OFF_DELAY;
                    set_timer_relative(self.on_ms, notifications::TIMER_MASK);
                }
                // Forth and later presses: Increment pattern every second.
                _ => {
                    self.on = true;
                    self.on_ms = ON_DELAY;
                    self.off_ms = 0;
                    set_timer_relative(self.on_ms, notifications::TIMER_MASK);
                }
            }
            Ok(self.update_leds()?)
        } else {
            // This is a "first" press outside of the quick press time window.
            // Make sure the LEDSs are on and increment the pattern.
            self.quick = 0;
            self.on = true;
            self.on_ms = 0;
            self.off_ms = 0;
            Ok(self.increment()?)
        }
    }
}

impl idl::InOrderButtonImpl for ButtonServer {
    /// Simulate a button press
    fn press(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u8, RequestError<ButtonError>> {
        self.handle_button_press()?;
        Ok(self.rgb)
    }

    fn off(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<(), RequestError<ButtonError>> {
        self.rgb = 0;
        let _ = self.update_leds()?;
        Ok(())
    }

    fn set(
        &mut self,
        _: &userlib::RecvMessage,
        rgb: u8,
    ) -> Result<(), RequestError<ButtonError>> {
        if rgb >= 8 {
            Err(ButtonError::InvalidValue.into())
        } else {
            self.rgb = rgb % 8;
            let _ = self.update_leds()?;
            Ok(())
        }
    }

    fn blink(
        &mut self,
        _: &userlib::RecvMessage,
        on: u32,
        off: u32,
    ) -> Result<(), RequestError<ButtonError>> {
        self.on_ms = on;
        self.off_ms = off;
        self.on = true;
        set_timer_relative(self.on_ms, notifications::TIMER_MASK);
        let _ = self.update_leds()?;
        Ok(())
    }
}

impl NotificationHandler for ButtonServer {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK + notifications::BUTTON_IRQ_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if (bits & notifications::TIMER_MASK) != 0 {
            self.timer_expiry()
        }

        if (bits & notifications::BUTTON_IRQ_MASK) != 0 {
            let detected = self
                .gpio
                .pint_op(
                    BUTTON_PINT_SLOT,
                    PintOp::Detected,
                    PintCondition::Falling,
                )
                .map_or(false, |v| v.unwrap_lite());
            let _ = self.gpio.pint_op(
                BUTTON_PINT_SLOT,
                PintOp::Clear,
                PintCondition::Falling,
            );
            let _ = self.gpio.pint_op(
                BUTTON_PINT_SLOT,
                PintOp::Clear,
                PintCondition::Status,
            );
            if detected {
                let _ = self.handle_button_press();
            }
            sys_irq_control(notifications::BUTTON_IRQ_MASK, true);
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0u8; INCOMING_SIZE];

    let gpio_driver = GPIO.get_task_id();
    setup_pins(gpio_driver).unwrap_lite();
    let gpio = drv_lpc55_gpio_api::Pins::from(gpio_driver);

    let mut server = ButtonServer {
        gpio,
        quick: 0,
        last_button_press: 0,
        on: true, // LEDs are on
        on_ms: 0,
        off_ms: 0,  // timer inactive, with on_ms > 0, increment
        rgb: 0b111, // start with all LEDs on
    };

    // Assume the normal case where the PINT has been reset and no other
    // task has fiddled our bits.
    // We're not clearing any state from a possible task restart.
    let _ = server.gpio.pint_op(
        BUTTON_PINT_SLOT,
        PintOp::Enable,
        PintCondition::Falling,
    );
    sys_irq_control(notifications::BUTTON_IRQ_MASK, true);

    let _ = server.update_leds();
    if server.on && server.on_ms > 0 {
        set_timer_relative(server.on_ms, notifications::TIMER_MASK);
    } else if !server.on && server.off_ms > 0 {
        set_timer_relative(server.off_ms, notifications::TIMER_MASK);
    }

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use crate::ButtonError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));
