use crate::bsp::{self, Led1};
use bsp::Bsp;
use rp2040_hal::pac;

const LED_GPIO_ID: usize = 25;

pub struct BspImpl {
    sio: pac::SIO,
}

impl Bsp for BspImpl {
    type Led = Led;

    fn new() -> Self {
        Self {
            // SAFETY: this probably isn't actually safe and we should think of a better approach
            sio: unsafe { pac::SIO::steal() },
        }
    }

    fn enable_led_pins(&self) {
        // enable the gpio SIO for the GPIO

        // setup io bank with sio
        let io_bank0 = unsafe { pac::IO_BANK0::steal() };
        io_bank0
            .gpio(LED_GPIO_ID)
            .gpio_ctrl()
            .modify(|_, w| w.funcsel().sio());

        // output enable
        self.sio
            .gpio_oe_set()
            .write(|w| unsafe { w.bits(1 << LED_GPIO_ID) });

        self.led_off(Led1::Zero);
    }

    fn led_on(&self, led: Self::Led) {
        match led {
            Led1::Zero => self
                .sio
                .gpio_out_set()
                .write(|w| unsafe { w.bits(1 << LED_GPIO_ID) }),
        }
    }

    fn led_off(&self, led: Self::Led) {
        match led {
            Led1::Zero => self
                .sio
                .gpio_out_clr()
                .write(|w| unsafe { w.bits(1 << LED_GPIO_ID) }),
        }
    }

    fn led_toggle(&self, led: Self::Led) {
        match led {
            Led1::Zero => self
                .sio
                .gpio_out_xor()
                .write(|w| unsafe { w.bits(1 << LED_GPIO_ID) }),
        }
    }
}

type Led = Led1;
