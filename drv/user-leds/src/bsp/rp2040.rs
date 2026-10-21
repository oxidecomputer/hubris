use crate::bsp::{self, Led1};
use bsp::Bsp;
use drv_rp2040_gpio_api::{Direction, Gpio, Pins};

userlib::task_slot!(GPIO, gpio_driver);

const LED_GPIO_ID: usize = 25;
const LED_GPIO: Gpio = Gpio::Gpio25;

pub struct BspImpl {
    pins: Pins,
}

impl Bsp for BspImpl {
    type Led = Led;

    fn new() -> Self {
        Self {
            pins: Pins::from(GPIO.get_task_id()),
        }
    }

    fn enable_led_pins(&self) {
        self.pins.set_dir(LED_GPIO, Direction::Out);
        // enable the gpio SIO for the GPIO

        self.led_off(Led1::Zero);
    }

    fn led_on(&self, led: Self::Led) {
        match led {
            Led1::Zero => self.pins.set_pin(LED_GPIO),
        }
    }

    fn led_off(&self, led: Self::Led) {
        match led {
            Led1::Zero => self.pins.clear_pin(LED_GPIO),
        }
    }

    fn led_toggle(&self, led: Self::Led) {
        match led {
            Led1::Zero => self.pins.toggle_pin(LED_GPIO),
        }
    }
}

type Led = Led1;
