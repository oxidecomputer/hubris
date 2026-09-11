// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::bsp::Bsp;
use userlib::task_slot;

task_slot!(GPIO, gpio_driver);

pub struct BspImpl;

impl Bsp for BspImpl {
    type Led = board::Led;

    fn enable_led_pins() {
        use drv_lpc55_gpio_api::*;

        let gpio_driver = GPIO.get_task_id();
        let gpio_driver = Pins::from(gpio_driver);

        // Both LEDs are active low -- so they will light when we set the
        // direction of the pin if we don't explicitly turn them off first
        for pin in board::PINS {
            gpio_driver.iocon_configure(
                *pin,
                AltFn::Alt0,
                Mode::NoPull,
                Slew::Standard,
                Invert::Disable,
                Digimode::Digital,
                Opendrain::Normal,
                None,
            );
            gpio_driver.set_val(*pin, board::LED_OFF_VAL);
            gpio_driver.set_dir(*pin, Direction::Output);
        }
    }

    fn led_on(led: Self::Led) {
        let gpio_driver = GPIO.get_task_id();
        let gpio_driver = drv_lpc55_gpio_api::Pins::from(gpio_driver);

        let pin = led_gpio_num(led);
        gpio_driver.set_val(pin, board::LED_ON_VAL);
    }

    fn led_off(led: Self::Led) {
        let gpio_driver = GPIO.get_task_id();
        let gpio_driver = drv_lpc55_gpio_api::Pins::from(gpio_driver);

        let pin = led_gpio_num(led);
        gpio_driver.set_val(pin, board::LED_OFF_VAL);
    }

    fn led_toggle(led: Self::Led) {
        use userlib::UnwrapLite;

        let gpio_driver = GPIO.get_task_id();
        let gpio_driver = drv_lpc55_gpio_api::Pins::from(gpio_driver);

        let pin = led_gpio_num(led);
        gpio_driver.toggle(pin).unwrap_lite();
    }
}

mod board {
    use crate::bsp::Led2;

    pub type Led = Led2;

    #[cfg(target_board = "lpcxpresso55s69")]
    pub const PINS: &[drv_lpc55_gpio_api::Pin] = &[
        drv_lpc55_gpio_api::Pin::PIO1_6,
        drv_lpc55_gpio_api::Pin::PIO1_4,
    ];

    #[cfg(any(target_board = "rot-carrier-1", target_board = "rot-carrier-2"))]
    pub const PINS: &[drv_lpc55_gpio_api::Pin] = &[
        drv_lpc55_gpio_api::Pin::PIO0_15,
        drv_lpc55_gpio_api::Pin::PIO0_31,
    ];

    pub use levels::{LED_OFF_VAL, LED_ON_VAL};

    #[cfg(target_board = "lpcxpresso55s69")]
    mod levels {
        pub const LED_OFF_VAL: drv_lpc55_gpio_api::Value =
            drv_lpc55_gpio_api::Value::One;
        pub const LED_ON_VAL: drv_lpc55_gpio_api::Value =
            drv_lpc55_gpio_api::Value::Zero;
    }

    #[cfg(any(target_board = "rot-carrier-1", target_board = "rot-carrier-2"))]
    mod levels {
        pub const LED_OFF_VAL: drv_lpc55_gpio_api::Value =
            drv_lpc55_gpio_api::Value::Zero;
        pub const LED_ON_VAL: drv_lpc55_gpio_api::Value =
            drv_lpc55_gpio_api::Value::One;
    }
}

const fn led_gpio_num(led: board::Led) -> drv_lpc55_gpio_api::Pin {
    board::PINS[led as usize]
}
