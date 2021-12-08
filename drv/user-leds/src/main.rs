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

#![no_std]
#![no_main]

use userlib::*;
use zerocopy::AsBytes;

#[derive(FromPrimitive)]
enum Op {
    On = 1,
    Off = 2,
    Toggle = 3,
}

cfg_if::cfg_if! {
    // Target boards with 4 leds
    if #[cfg(any(target_board = "gemini-bu-1", target_board = "gimletlet-2"))] {
        #[derive(FromPrimitive)]
        enum Led {
            Zero = 0,
            One = 1,
            Two = 2,
            Three = 3,
        }
    }
    // Target boards with 3 leds
    else if #[cfg(any(target_board = "nucleo-h753zi", target_board = "nucleo-h743zi2"))] {
        #[derive(FromPrimitive)]
        enum Led {
            Zero = 0,
            One = 1,
            Two = 2,
        }
    }
    // Target boards with 2 leds -> the rest
    else {
        #[derive(FromPrimitive)]
        enum Led {
            Zero = 0,
            One = 1,
        }
    }
}

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

#[export_name = "main"]
fn main() -> ! {
    enable_led_pins();

    // Field messages.
    // Ensure our buffer is aligned properly for a u32 by declaring it as one.
    let mut buffer = 0u32;
    loop {
        hl::recv_without_notification(
            buffer.as_bytes_mut(),
            |op, msg| -> Result<(), ResponseCode> {
                // Every incoming message uses the same payload type and
                // response type: it's always u32 -> (). So we can do the
                // check-and-convert here:
                let (msg, caller) =
                    msg.fixed::<u32, ()>().ok_or(ResponseCode::BadArg)?;

                // Every incoming message has the same permitted range, as well.
                let led = Led::from_u32(*msg).ok_or(ResponseCode::BadArg)?;

                match op {
                    Op::On => led_on(led),
                    Op::Off => led_off(led),
                    Op::Toggle => led_toggle(led),
                }

                caller.reply(());
                Ok(())
            },
        );
    }
}

///////////////////////////////////////////////////////////////////////////////
// The STM32F3/4 specific bits.
//
// STM32F3/4 are the only platforms that still pokes the GPIOs directly, without an
// intermediary.

cfg_if::cfg_if! {
    if #[cfg(any(feature = "stm32f4", feature = "stm32f3"))] {
        task_slot!(RCC, rcc_driver);
    }
}

// The types returned are different and so we just use a macro
// here to avoid repeating the cfg block when used below
#[cfg(feature = "stm32f3")]
macro_rules! gpio {
    () => {
        unsafe { &*stm32f3::stm32f303::GPIOE::ptr() }
    };
}
#[cfg(feature = "stm32f4")]
macro_rules! gpio {
    () => {
        unsafe { &*stm32f4::stm32f407::GPIOD::ptr() }
    };
}

#[cfg(any(feature = "stm32f3", feature = "stm32f4"))]
fn enable_led_pins() {
    // This assumes an STM32F4DISCOVERY board, where the LEDs are on D12 and
    // D13 OR an STM32F3DISCOVERY board, where the LEDs are on E8 and E9.

    // Contact the RCC driver to get power turned on for GPIOD/E.
    let rcc_driver = RCC.get_task_id();
    const ENABLE_CLOCK: u16 = 1;

    #[cfg(feature = "stm32f3")]
    let gpio_pnum = 21; // see bits in AHBENR
    #[cfg(feature = "stm32f4")]
    let gpio_pnum = 3; // see bits in AHB1ENR

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
    let gpio_moder = &gpio!().moder;

    #[cfg(feature = "stm32f3")]
    gpio_moder.modify(|_, w| w.moder8().output().moder9().output());
    #[cfg(feature = "stm32f4")]
    gpio_moder.modify(|_, w| w.moder12().output().moder13().output());
}

#[cfg(any(feature = "stm32f3", feature = "stm32f4"))]
fn led_on(led: Led) {
    let gpio = gpio!();

    match led {
        #[cfg(feature = "stm32f3")]
        Led::Zero => gpio.bsrr.write(|w| w.bs8().set_bit()),
        #[cfg(feature = "stm32f3")]
        Led::One => gpio.bsrr.write(|w| w.bs9().set_bit()),

        #[cfg(feature = "stm32f4")]
        Led::Zero => gpio.bsrr.write(|w| w.bs12().set_bit()),
        #[cfg(feature = "stm32f4")]
        Led::One => gpio.bsrr.write(|w| w.bs13().set_bit()),
    }
}

#[cfg(any(feature = "stm32f3", feature = "stm32f4"))]
fn led_off(led: Led) {
    let gpio = gpio!();

    match led {
        #[cfg(feature = "stm32f3")]
        Led::Zero => gpio.bsrr.write(|w| w.br8().set_bit()),
        #[cfg(feature = "stm32f3")]
        Led::One => gpio.bsrr.write(|w| w.br9().set_bit()),

        #[cfg(feature = "stm32f4")]
        Led::Zero => gpio.bsrr.write(|w| w.br12().set_bit()),
        #[cfg(feature = "stm32f4")]
        Led::One => gpio.bsrr.write(|w| w.br13().set_bit()),
    }
}

#[cfg(any(feature = "stm32f3", feature = "stm32f4"))]
fn led_toggle(led: Led) {
    let gpio = gpio!();

    match led {
        #[cfg(feature = "stm32f3")]
        Led::Zero => {
            if gpio.odr.read().odr8().bit() {
                gpio.bsrr.write(|w| w.br8().set_bit())
            } else {
                gpio.bsrr.write(|w| w.bs8().set_bit())
            }
        }
        #[cfg(feature = "stm32f3")]
        Led::One => {
            if gpio.odr.read().odr9().bit() {
                gpio.bsrr.write(|w| w.br9().set_bit())
            } else {
                gpio.bsrr.write(|w| w.bs9().set_bit())
            }
        }

        #[cfg(feature = "stm32f4")]
        Led::Zero => {
            if gpio.odr.read().odr12().bit() {
                gpio.bsrr.write(|w| w.br12().set_bit())
            } else {
                gpio.bsrr.write(|w| w.bs12().set_bit())
            }
        }
        #[cfg(feature = "stm32f4")]
        Led::One => {
            if gpio.odr.read().odr13().bit() {
                gpio.bsrr.write(|w| w.br13().set_bit())
            } else {
                gpio.bsrr.write(|w| w.bs13().set_bit())
            }
        }
    }
}

///////////////////////////////////////////////////////////////////////////////
// The STM32H7 specific bits.
//

cfg_if::cfg_if! {
    if #[cfg(feature = "stm32h7")] {
        task_slot!(GPIO, gpio_driver);

        cfg_if::cfg_if! {
            if #[cfg(target_board = "stm32h7b3i-dk")] {
                // STM32H7B3 DISCOVERY kit: LEDs are on G2 and G11.
                const LEDS: &[(drv_stm32h7_gpio_api::PinSet, bool)] = &[
                    (drv_stm32h7_gpio_api::Port::G.pin(2), true),
                    (drv_stm32h7_gpio_api::Port::G.pin(11), true),
                ];
            } else if #[cfg(any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"))] {
                // Nucleo boards: LEDs are on PB0, PB14 and PE1.
                const LEDS: &[(drv_stm32h7_gpio_api::PinSet, bool)] = &[
                    (drv_stm32h7_gpio_api::Port::B.pin(0), false),
                    (drv_stm32h7_gpio_api::Port::B.pin(14), false),
                    (drv_stm32h7_gpio_api::Port::E.pin(1), false),
                ];
            } else if #[cfg(target_board = "gemini-bu-1")] {
                // Gemini bringup SP: LEDs are on PI8, PI9, PI10 and PI11.
                const LEDS: &[(drv_stm32h7_gpio_api::PinSet, bool)] = &[
                    (drv_stm32h7_gpio_api::Port::I.pin(8), false),
                    (drv_stm32h7_gpio_api::Port::I.pin(9), false),
                    (drv_stm32h7_gpio_api::Port::I.pin(10), false),
                    (drv_stm32h7_gpio_api::Port::I.pin(11), false),
                ];
            } else if #[cfg(target_board = "gimletlet-2")] {
                // Glorified gimletlet SP: LEDs are on PG2-5
                const LEDS: &[(drv_stm32h7_gpio_api::PinSet, bool)] = &[
                    (drv_stm32h7_gpio_api::Port::G.pin(2), false),
                    (drv_stm32h7_gpio_api::Port::G.pin(3), false),
                    (drv_stm32h7_gpio_api::Port::G.pin(4), false),
                    (drv_stm32h7_gpio_api::Port::G.pin(5), false),
                ];
            } else {
                compile_error!("no LED mapping for unknown board");
            }
        }
    }
}

#[cfg(feature = "stm32h7")]
fn enable_led_pins() {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = Gpio::from(gpio_driver);

    for &(pinset, active_low) in LEDS {
        // Make sure LEDs are initially off.
        gpio_driver.set_to(pinset, active_low).unwrap();
        // Make them outputs.
        gpio_driver
            .configure_output(
                pinset,
                OutputType::PushPull,
                Speed::High,
                Pull::None,
            )
            .unwrap();
    }
}

#[cfg(feature = "stm32h7")]
fn led_info(led: Led) -> (drv_stm32h7_gpio_api::PinSet, bool) {
    match led {
        Led::Zero => LEDS[0],
        Led::One => LEDS[1],
        #[cfg(any(target_board = "gemini-bu-1", target_board = "gimletlet-2", target_board = "nucleo-h753zi", target_board = "nucleo-h743zi2"))]
        Led::Two => LEDS[2],
        #[cfg(any(target_board = "gemini-bu-1", target_board = "gimletlet-2"))]
        Led::Three => LEDS[3],
    }
}

#[cfg(feature = "stm32h7")]
fn led_on(led: Led) {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = Gpio::from(gpio_driver);

    let (pinset, active_low) = led_info(led);
    gpio_driver.set_to(pinset, !active_low).unwrap();
}

#[cfg(feature = "stm32h7")]
fn led_off(led: Led) {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = Gpio::from(gpio_driver);

    let (pinset, active_low) = led_info(led);

    gpio_driver.set_to(pinset, active_low).unwrap();
}

#[cfg(feature = "stm32h7")]
fn led_toggle(led: Led) {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = Gpio::from(gpio_driver);

    let pinset = led_info(led).0;
    gpio_driver.toggle(pinset.port, pinset.pin_mask).unwrap();
}

///////////////////////////////////////////////////////////////////////////////
// The LPC55 specific bits.

cfg_if::cfg_if! {
    if #[cfg(feature = "lpc55")] {
        task_slot!(GPIO, gpio_driver);

        cfg_if::cfg_if! {
            if #[cfg(target_board = "lpcxpresso55s69")] {
                const LED_ZERO_PIN: drv_lpc55_gpio_api::Pin = drv_lpc55_gpio_api::Pin::PIO1_6;
                const LED_ONE_PIN: drv_lpc55_gpio_api::Pin = drv_lpc55_gpio_api::Pin::PIO1_4;

                // xpressoboard is active low LEDS
                const LED_OFF_VAL: drv_lpc55_gpio_api::Value = drv_lpc55_gpio_api::Value::One;
                const LED_ON_VAL: drv_lpc55_gpio_api::Value = drv_lpc55_gpio_api::Value::Zero;
            } else if #[cfg(target_board = "gemini-bu-rot-1")] {
                const LED_ZERO_PIN: drv_lpc55_gpio_api::Pin = drv_lpc55_gpio_api::Pin::PIO0_15;
                const LED_ONE_PIN: drv_lpc55_gpio_api::Pin = drv_lpc55_gpio_api::Pin::PIO0_31;

                // gemini bu board is standard values
                const LED_OFF_VAL: drv_lpc55_gpio_api::Value = drv_lpc55_gpio_api::Value::Zero;
                const LED_ON_VAL: drv_lpc55_gpio_api::Value = drv_lpc55_gpio_api::Value::One;
            } else {
                compile_error!("no LED mapping for unknown board");
            }
        }
    }
}

#[cfg(feature = "lpc55")]
const fn led_gpio_num(led: Led) -> drv_lpc55_gpio_api::Pin {
    match led {
        Led::Zero => LED_ZERO_PIN,
        Led::One => LED_ONE_PIN,
    }
}

#[cfg(feature = "lpc55")]
fn enable_led_pins() {
    use drv_lpc55_gpio_api::*;

    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = Gpio::from(gpio_driver);

    gpio_driver
        .iocon_configure(
            LED_ZERO_PIN,
            AltFn::Alt0,
            Mode::NoPull,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        )
        .unwrap();

    gpio_driver
        .iocon_configure(
            LED_ONE_PIN,
            AltFn::Alt0,
            Mode::NoPull,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        )
        .unwrap();

    // Both LEDs are active low -- so they will light when we set the
    // direction of the pin if we don't explicitly turn them off first
    led_off(Led::Zero);
    led_off(Led::One);

    gpio_driver
        .set_dir(LED_ZERO_PIN, Direction::Output)
        .unwrap();
    gpio_driver.set_dir(LED_ONE_PIN, Direction::Output).unwrap();
}

#[cfg(feature = "lpc55")]
fn led_on(led: Led) {
    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = drv_lpc55_gpio_api::Gpio::from(gpio_driver);

    let pin = led_gpio_num(led);
    gpio_driver.set_val(pin, LED_ON_VAL).unwrap();
}

#[cfg(feature = "lpc55")]
fn led_off(led: Led) {
    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = drv_lpc55_gpio_api::Gpio::from(gpio_driver);

    let pin = led_gpio_num(led);
    gpio_driver.set_val(pin, LED_OFF_VAL).unwrap();
}

#[cfg(feature = "lpc55")]
fn led_toggle(led: Led) {
    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = drv_lpc55_gpio_api::Gpio::from(gpio_driver);

    let pin = led_gpio_num(led);
    gpio_driver.toggle(pin).unwrap();
}
