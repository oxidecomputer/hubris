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

use idol_runtime::RequestError;
use userlib::*;

cfg_if::cfg_if! {
    // Target boards with 4 leds
    if #[cfg(any(
            target_board = "gemini-bu-1",
            target_board = "gimletlet-1",
            target_board = "gimletlet-2"
        ))] {
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
    // Target boards with 1 led
    else if #[cfg(any(target_board = "stm32g031", target_board = "stm32g070", target_board = "stm32g0b1"))] {
        #[derive(FromPrimitive)]
        enum Led {
            Zero = 0,
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
pub enum LedError {
    NotPresent = 1,
}

impl From<LedError> for u16 {
    fn from(rc: LedError) -> Self {
        rc as u16
    }
}

struct ServerImpl;

impl idl::InOrderUserLedsImpl for ServerImpl {
    fn led_on(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<LedError>> {
        let led = Led::from_usize(index).ok_or(LedError::NotPresent)?;
        led_on(led);
        Ok(())
    }
    fn led_off(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<LedError>> {
        let led = Led::from_usize(index).ok_or(LedError::NotPresent)?;
        led_off(led);
        Ok(())
    }
    fn led_toggle(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<(), RequestError<LedError>> {
        let led = Led::from_usize(index).ok_or(LedError::NotPresent)?;
        led_toggle(led);
        Ok(())
    }
}

#[export_name = "main"]
fn main() -> ! {
    enable_led_pins();

    // Handle messages.
    let mut incoming = [0u8; idl::INCOMING_SIZE];
    let mut serverimpl = ServerImpl;
    loop {
        idol_runtime::dispatch(&mut incoming, &mut serverimpl);
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
    use zerocopy::AsBytes;

    // This assumes an STM32F4DISCOVERY board, where the LEDs are on D12 and
    // D13 OR an STM32F3DISCOVERY board, where the LEDs are on E8 and E9.

    // Contact the RCC driver to get power turned on for GPIOD/E.
    let rcc_driver = RCC.get_task_id();
    const ENABLE_CLOCK: u16 = 1;

    #[cfg(feature = "stm32f3")]
    let gpio_pnum: u32 = 21; // see bits in AHBENR
    #[cfg(feature = "stm32f4")]
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
// The STM32G0 specific bits.
//

cfg_if::cfg_if! {
    if #[cfg(feature = "stm32g0")] {
        task_slot!(SYS, sys);

        const LEDS: &[(drv_stm32xx_sys_api::PinSet, bool)] = &[
        {
            cfg_if::cfg_if! {
                if #[cfg(target_board = "stm32g031")] {
                    (drv_stm32xx_sys_api::Port::C.pin(6), true)
                } else {
                    (drv_stm32xx_sys_api::Port::A.pin(5), true)
                }
            }
        },
        ];
    }
}

#[cfg(feature = "stm32g0")]
fn enable_led_pins() {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    for &(pinset, active_low) in LEDS {
        // Make sure LEDs are initially off.
        sys.gpio_set_to(pinset, active_low).unwrap();
        // Make them outputs.
        sys.gpio_configure_output(
            pinset,
            OutputType::PushPull,
            Speed::High,
            Pull::None,
        )
        .unwrap();
    }
}

#[cfg(feature = "stm32g0")]
fn led_info(led: Led) -> (drv_stm32xx_sys_api::PinSet, bool) {
    match led {
        Led::Zero => LEDS[0],
    }
}

#[cfg(feature = "stm32g0")]
fn led_on(led: Led) {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    let (pinset, active_low) = led_info(led);
    sys.gpio_set_to(pinset, !active_low).unwrap();
}

#[cfg(feature = "stm32g0")]
fn led_off(led: Led) {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    let (pinset, active_low) = led_info(led);

    sys.gpio_set_to(pinset, active_low).unwrap();
}

#[cfg(feature = "stm32g0")]
fn led_toggle(led: Led) {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    let pinset = led_info(led).0;
    sys.gpio_toggle(pinset.port, pinset.pin_mask).unwrap();
}

///////////////////////////////////////////////////////////////////////////////
// The STM32H7 specific bits.
//

cfg_if::cfg_if! {
    if #[cfg(feature = "stm32h7")] {
        task_slot!(SYS, sys);

        cfg_if::cfg_if! {
            if #[cfg(target_board = "stm32h7b3i-dk")] {
                // STM32H7B3 DISCOVERY kit: LEDs are on G2 and G11.
                const LEDS: &[(drv_stm32xx_sys_api::PinSet, bool)] = &[
                    (drv_stm32xx_sys_api::Port::G.pin(2), true),
                    (drv_stm32xx_sys_api::Port::G.pin(11), true),
                ];
            } else if #[cfg(any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"))] {
                // Nucleo boards: LEDs are on PB0, PB14 and PE1.
                const LEDS: &[(drv_stm32xx_sys_api::PinSet, bool)] = &[
                    (drv_stm32xx_sys_api::Port::B.pin(0), false),
                    (drv_stm32xx_sys_api::Port::B.pin(14), false),
                    (drv_stm32xx_sys_api::Port::E.pin(1), false),
                ];
            } else if #[cfg(target_board = "gemini-bu-1")] {
                // Gemini bringup SP: LEDs are on PI8, PI9, PI10 and PI11.
                const LEDS: &[(drv_stm32xx_sys_api::PinSet, bool)] = &[
                    (drv_stm32xx_sys_api::Port::I.pin(8), false),
                    (drv_stm32xx_sys_api::Port::I.pin(9), false),
                    (drv_stm32xx_sys_api::Port::I.pin(10), false),
                    (drv_stm32xx_sys_api::Port::I.pin(11), false),
                ];
            } else if #[cfg(target_board = "gimletlet-1")] {
                // Original Gimletlet: LEDs are on PI8-11
                const LEDS: &[(drv_stm32h7_gpio_api::PinSet, bool)] = &[
                    (drv_stm32h7_gpio_api::Port::I.pin(8), false),
                    (drv_stm32h7_gpio_api::Port::I.pin(9), false),
                    (drv_stm32h7_gpio_api::Port::I.pin(10), false),
                    (drv_stm32h7_gpio_api::Port::I.pin(11), false),
                ];
            } else if #[cfg(target_board = "gimletlet-2")] {
                // Glorified gimletlet SP: LEDs are on PG2-5
                const LEDS: &[(drv_stm32xx_sys_api::PinSet, bool)] = &[
                    (drv_stm32xx_sys_api::Port::G.pin(2), false),
                    (drv_stm32xx_sys_api::Port::G.pin(3), false),
                    (drv_stm32xx_sys_api::Port::G.pin(4), false),
                    (drv_stm32xx_sys_api::Port::G.pin(5), false),
                ];
            } else {
                compile_error!("no LED mapping for unknown board");
            }
        }
    }
}

#[cfg(feature = "stm32h7")]
fn enable_led_pins() {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    for &(pinset, active_low) in LEDS {
        // Make sure LEDs are initially off.
        sys.gpio_set_to(pinset, active_low).unwrap();
        // Make them outputs.
        sys.gpio_configure_output(
            pinset,
            OutputType::PushPull,
            Speed::High,
            Pull::None,
        )
        .unwrap();
    }
}

#[cfg(feature = "stm32h7")]
fn led_info(led: Led) -> (drv_stm32xx_sys_api::PinSet, bool) {
    match led {
        Led::Zero => LEDS[0],
        Led::One => LEDS[1],
        #[cfg(any(
            target_board = "gemini-bu-1",
            target_board = "gimletlet-1",
            target_board = "gimletlet-2",
            target_board = "nucleo-h753zi",
            target_board = "nucleo-h743zi2"
        ))]
        Led::Two => LEDS[2],
        #[cfg(any(
            target_board = "gemini-bu-1",
            target_board = "gimletlet-1",
            target_board = "gimletlet-2"
        ))]
        Led::Three => LEDS[3],
    }
}

#[cfg(feature = "stm32h7")]
fn led_on(led: Led) {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    let (pinset, active_low) = led_info(led);
    sys.gpio_set_to(pinset, !active_low).unwrap();
}

#[cfg(feature = "stm32h7")]
fn led_off(led: Led) {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    let (pinset, active_low) = led_info(led);

    sys.gpio_set_to(pinset, active_low).unwrap();
}

#[cfg(feature = "stm32h7")]
fn led_toggle(led: Led) {
    use drv_stm32xx_sys_api::*;

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    let pinset = led_info(led).0;
    sys.gpio_toggle(pinset.port, pinset.pin_mask).unwrap();
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

mod idl {
    use super::LedError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
