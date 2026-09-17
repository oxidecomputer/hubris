// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The LPC55 specific bits.
//!
//! This is largely used by development boards, the main `oxide-rot-1` image
//! does not (as of 2026-09-14) feature user LEDs.

use crate::bsp::Bsp;
use drv_lpc55_gpio_api::{
    AltFn, Digimode, Direction, Invert, Mode, Opendrain, Pin, Pins, Slew,
};
use userlib::task_slot;

task_slot!(GPIO, gpio_driver);

pub struct BspImpl {
    gpio: Pins,
}

impl Bsp for BspImpl {
    type Led = board::Led;

    fn new() -> Self {
        Self {
            gpio: Pins::from(GPIO.get_task_id()),
        }
    }

    fn enable_led_pins(&self) {
        for pin in board::LEDS {
            self.gpio.iocon_configure(
                *pin,
                AltFn::Alt0,
                Mode::NoPull,
                Slew::Standard,
                Invert::Disable,
                Digimode::Digital,
                Opendrain::Normal,
                None,
            );
            self.gpio.set_val(*pin, board::LED_OFF_VAL);
            self.gpio.set_dir(*pin, Direction::Output);
        }
    }

    fn led_on(&self, led: Self::Led) {
        let pin = led_gpio_num(led);
        self.gpio.set_val(pin, board::LED_ON_VAL);
    }

    fn led_off(&self, led: Self::Led) {
        let pin = led_gpio_num(led);
        self.gpio.set_val(pin, board::LED_OFF_VAL);
    }

    fn led_toggle(&self, led: Self::Led) {
        use userlib::UnwrapLite;
        let pin = led_gpio_num(led);
        self.gpio.toggle(pin).unwrap_lite();
    }
}

mod board {
    use crate::bsp::Led2;
    use drv_lpc55_gpio_api::Pin;

    pub type Led = Led2;

    #[cfg(target_board = "lpcxpresso55s69")]
    pub const LEDS: &[Pin] = &[Pin::PIO1_6, Pin::PIO1_4];

    #[cfg(any(target_board = "rot-carrier-1", target_board = "rot-carrier-2"))]
    pub const LEDS: &[Pin] = &[Pin::PIO0_15, Pin::PIO0_31];

    #[cfg(target_board = "lpcxpresso55s69")]
    mod levels {
        use drv_lpc55_gpio_api::Value;
        pub const LED_OFF_VAL: Value = Value::One;
        pub const LED_ON_VAL: Value = Value::Zero;
    }

    #[cfg(any(target_board = "rot-carrier-1", target_board = "rot-carrier-2"))]
    mod levels {
        use drv_lpc55_gpio_api::Value;
        pub const LED_OFF_VAL: Value = Value::Zero;
        pub const LED_ON_VAL: Value = Value::One;
    }

    // Re-export the selected values
    pub use levels::{LED_OFF_VAL, LED_ON_VAL};
}

fn led_gpio_num(led: board::Led) -> Pin {
    const _: () = assert!(
        board::LEDS.len() == <board::Led as enum_map::Enum>::LENGTH,
        "LED count mismatch length vs enum"
    );
    board::LEDS[led as usize]
}
