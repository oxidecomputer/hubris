// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::bsp::{Bsp, Led2};
use userlib::task_slot;

///////////////////////////////////////////////////////////////////////////////
// The STM32F3/4 specific bits.
//
// STM32F3/4 are the only platforms that still pokes the GPIOs directly, without an
// intermediary.

task_slot!(RCC, rcc_driver);

pub struct BspImpl;

#[cfg(feature = "stm32f3")]
impl Bsp for BspImpl {
    type Led = Led2;

    fn enable_led_pins() {
        use zerocopy::IntoBytes;

        // This assumes an STM32F4DISCOVERY board, where the LEDs are on D12 and
        // D13 OR an STM32F3DISCOVERY board, where the LEDs are on E8 and E9.

        // Contact the RCC driver to get power turned on for GPIOD/E.
        let rcc_driver = RCC.get_task_id();
        const ENABLE_CLOCK: u16 = 1;

        let gpio_pnum: u32 = 21; // see bits in AHBENR

        let (code, _) = userlib::sys_send(
            rcc_driver,
            ENABLE_CLOCK,
            gpio_pnum.as_bytes(),
            &mut [],
            &[],
        );
        assert_eq!(code, 0);

        // Now, directly manipulate GPIOD/E.
        // TODO: this should go through a gpio driver probably.
        let gpio_moder = &unsafe { &*stm32f3::stm32f303::GPIOE::ptr() }.moder;
        gpio_moder.modify(|_, w| w.moder8().output().moder9().output());
    }

    fn led_on(led: Self::Led) {
        let gpio = unsafe { &*stm32f3::stm32f303::GPIOE::ptr() };

        match led {
            Self::Led::Zero => gpio.bsrr.write(|w| w.bs8().set_bit()),
            Self::Led::One => gpio.bsrr.write(|w| w.bs9().set_bit()),
        }
    }

    fn led_off(led: Self::Led) {
        let gpio = unsafe { &*stm32f3::stm32f303::GPIOE::ptr() };

        match led {
            Self::Led::Zero => gpio.bsrr.write(|w| w.br8().set_bit()),
            Self::Led::One => gpio.bsrr.write(|w| w.br9().set_bit()),
        }
    }

    fn led_toggle(led: Self::Led) {
        let gpio = unsafe { &*stm32f3::stm32f303::GPIOE::ptr() };

        match led {
            Self::Led::Zero => {
                if gpio.odr.read().odr8().bit() {
                    gpio.bsrr.write(|w| w.br8().set_bit())
                } else {
                    gpio.bsrr.write(|w| w.bs8().set_bit())
                }
            }
            Self::Led::One => {
                if gpio.odr.read().odr9().bit() {
                    gpio.bsrr.write(|w| w.br9().set_bit())
                } else {
                    gpio.bsrr.write(|w| w.bs9().set_bit())
                }
            }
        }
    }
}

#[cfg(feature = "stm32f4")]
impl Bsp for BspImpl {
    type Led = Led2;

    fn enable_led_pins() {
        use zerocopy::IntoBytes;

        // This assumes an STM32F4DISCOVERY board, where the LEDs are on D12 and
        // D13 OR an STM32F3DISCOVERY board, where the LEDs are on E8 and E9.

        // Contact the RCC driver to get power turned on for GPIOD/E.
        let rcc_driver = RCC.get_task_id();
        const ENABLE_CLOCK: u16 = 1;

        let gpio_pnum: u32 = 3; // see bits in AHB1ENR

        let (code, _) = userlib::sys_send(
            rcc_driver,
            ENABLE_CLOCK,
            gpio_pnum.as_bytes(),
            &mut [],
            &[],
        );
        assert_eq!(code, 0);

        // Now, directly manipulate GPIOD/E.
        // TODO: this should go through a gpio driver probably.
        let gpio_moder = &unsafe { &*stm32f4::stm32f407::GPIOD::ptr() }.moder;
        gpio_moder.modify(|_, w| w.moder12().output().moder13().output());
    }

    fn led_on(led: Self::Led) {
        let gpio = unsafe { &*stm32f4::stm32f407::GPIOD::ptr() };

        match led {
            Self::Led::Zero => gpio.bsrr.write(|w| w.bs12().set_bit()),
            Self::Led::One => gpio.bsrr.write(|w| w.bs13().set_bit()),
        }
    }

    fn led_off(led: Self::Led) {
        let gpio = unsafe { &*stm32f4::stm32f407::GPIOD::ptr() };

        match led {
            Self::Led::Zero => gpio.bsrr.write(|w| w.br12().set_bit()),
            Self::Led::One => gpio.bsrr.write(|w| w.br13().set_bit()),
        }
    }

    fn led_toggle(led: Self::Led) {
        let gpio = unsafe { &*stm32f4::stm32f407::GPIOD::ptr() };

        match led {
            Self::Led::Zero => {
                if gpio.odr.read().odr12().bit() {
                    gpio.bsrr.write(|w| w.br12().set_bit())
                } else {
                    gpio.bsrr.write(|w| w.bs12().set_bit())
                }
            }
            Self::Led::One => {
                if gpio.odr.read().odr13().bit() {
                    gpio.bsrr.write(|w| w.br13().set_bit())
                } else {
                    gpio.bsrr.write(|w| w.bs13().set_bit())
                }
            }
        }
    }
}
