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

#[derive(FromPrimitive)]
enum Led {
    Zero = 0,
    One = 1,
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
// The STM32F4 specific bits.
//
// STM32F4 is the only platform that still pokes the GPIOs directly, without an
// intermediary.

cfg_if::cfg_if! {
    if #[cfg(feature = "stm32f4")] {
        cfg_if::cfg_if! {
            if #[cfg(feature = "standalone")] {
                const RCC: Task = Task::anonymous;
            } else {
                const RCC: Task = Task::rcc_driver;
            }
        }
    }
}

#[cfg(feature = "stm32f4")]
fn enable_led_pins() {
    // This assumes an STM32F4DISCOVERY board, where the LEDs are on D12 and
    // D13.

    // Contact the RCC driver to get power turned on for GPIOD.
    let rcc_driver =
        TaskId::for_index_and_gen(RCC as usize, Generation::default());
    const ENABLE_CLOCK: u16 = 1;
    let gpiod_pnum = 3; // see bits in AHB1ENR
    let (code, _) = userlib::sys_send(
        rcc_driver,
        ENABLE_CLOCK,
        gpiod_pnum.as_bytes(),
        &mut [],
        &[],
    );
    assert_eq!(code, 0);

    // Now, directly manipulate GPIOD.
    // TODO: this should go through a gpio driver probably.
    let gpiod = unsafe { &*stm32f4::stm32f407::GPIOD::ptr() };
    gpiod
        .moder
        .modify(|_, w| w.moder12().output().moder13().output());
}

#[cfg(feature = "stm32f4")]
fn led_on(led: Led) {
    let gpiod = unsafe { &*stm32f4::stm32f407::GPIOD::ptr() };

    match led {
        Led::Zero => gpiod.bsrr.write(|w| w.bs12().set_bit()),
        Led::One => gpiod.bsrr.write(|w| w.bs13().set_bit()),
    }
}

#[cfg(feature = "stm32f4")]
fn led_off(led: Led) {
    let gpiod = unsafe { &*stm32f4::stm32f407::GPIOD::ptr() };

    match led {
        Led::Zero => gpiod.bsrr.write(|w| w.br12().set_bit()),
        Led::One => gpiod.bsrr.write(|w| w.br13().set_bit()),
    }
}

#[cfg(feature = "stm32f4")]
fn led_toggle(led: Led) {
    let gpiod = unsafe { &*stm32f4::stm32f407::GPIOD::ptr() };

    match led {
        Led::Zero => {
            if gpiod.odr.read().odr12().bit() {
                gpiod.bsrr.write(|w| w.br12().set_bit())
            } else {
                gpiod.bsrr.write(|w| w.bs12().set_bit())
            }
        }
        Led::One => {
            if gpiod.odr.read().odr13().bit() {
                gpiod.bsrr.write(|w| w.br13().set_bit())
            } else {
                gpiod.bsrr.write(|w| w.bs13().set_bit())
            }
        }
    }
}

///////////////////////////////////////////////////////////////////////////////
// The STM32H7 specific bits.
//

cfg_if::cfg_if! {
    if #[cfg(feature = "stm32h7")] {
        cfg_if::cfg_if! {
            if #[cfg(feature = "standalone")] {
                const GPIO: Task = Task::anonymous;
            } else {
                const GPIO: Task = Task::gpio_driver;
            }
        }

        cfg_if::cfg_if! {
            if #[cfg(target_board = "stm32h7b3i-dk")] {
                // STM32H7B3 DISCOVERY kit; LEDs are on G2 and G11.
                const LED_PORT: drv_stm32h7_gpio_api::Port =
                    drv_stm32h7_gpio_api::Port::G;
                const LED_MASK_0: u16 = 1 << 2;
                const LED_MASK_1: u16 = 1 << 11;
            } else if #[cfg(target_board = "nucleo-h743zi2")] {
                // Nucleo board, LEDs are on B0 and B14.
                const LED_PORT: drv_stm32h7_gpio_api::Port =
                    drv_stm32h7_gpio_api::Port::B;
                const LED_MASK_0: u16 = 1 << 0;
                const LED_MASK_1: u16 = 1 << 14;
            } else {
                compile_error!("no LED mapping for unknown board");
            }
        }
    }
}

#[cfg(feature = "stm32h7")]
fn enable_led_pins() {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);

    gpio_driver
        .configure(
            LED_PORT,
            LED_MASK_0 | LED_MASK_1,
            Mode::Output,
            OutputType::PushPull,
            Speed::High,
            Pull::None,
            Alternate::AF0,
        )
        .unwrap();
}

#[cfg(feature = "stm32h7")]
fn led_on(led: Led) {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);

    let set_mask = match led {
        Led::Zero => LED_MASK_0,
        Led::One => LED_MASK_1,
    };
    gpio_driver.set_reset(LED_PORT, set_mask, 0).unwrap();
}

#[cfg(feature = "stm32h7")]
fn led_off(led: Led) {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);

    let reset_mask = match led {
        Led::Zero => LED_MASK_0,
        Led::One => LED_MASK_1,
    };
    gpio_driver.set_reset(LED_PORT, 0, reset_mask).unwrap();
}

#[cfg(feature = "stm32h7")]
fn led_toggle(led: Led) {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);
    let mask = match led {
        Led::Zero => LED_MASK_0,
        Led::One => LED_MASK_1,
    };

    gpio_driver.toggle(LED_PORT, mask).unwrap();
}

///////////////////////////////////////////////////////////////////////////////
// The LPC55 specific bits.

cfg_if::cfg_if! {
    if #[cfg(feature = "lpc55")] {
        cfg_if::cfg_if! {
            if #[cfg(feature = "standalone")] {
                const GPIO: Task = Task::anonymous;
            } else {
                const GPIO: Task = Task::gpio_driver;
            }
        }
    }
}

#[cfg(feature = "lpc55")]
fn enable_led_pins() {
    use lpc55_pac as device;

    // This assumes the LPCXpresso55S board, where the LEDs are on (abstract
    // pins) 36 and 38.
    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    const SET_DIR: u16 = 1;

    // Ideally this would be done in another driver but given what svd2rust
    // generates it's a nightmare to do this via pin indexing only and
    // also have some degree of safety. If the pins aren't in digital mode
    // the GPIO toggling will work but reading the value won't
    let iocon = unsafe { &*device::IOCON::ptr() };
    iocon.pio1_4.modify(|_, w| w.digimode().digital());
    iocon.pio1_6.modify(|_, w| w.digimode().digital());

    // red led
    let (code, _) =
        userlib::sys_send(gpio_driver, SET_DIR, &[38, 1], &mut [], &[]);
    assert_eq!(code, 0);

    // blue led
    let (code, _) =
        userlib::sys_send(gpio_driver, SET_DIR, &[36, 1], &mut [], &[]);
    assert_eq!(code, 0);
}

#[cfg(feature = "lpc55")]
fn led_on(led: Led) {
    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    const SET_VAL: u16 = 2;
    let idx = match led {
        Led::Zero => 36,
        Led::One => 38,
    };
    let (code, _) =
        userlib::sys_send(gpio_driver, SET_VAL, &[idx, 0], &mut [], &[]);
    assert_eq!(code, 0);
}

#[cfg(feature = "lpc55")]
fn led_off(led: Led) {
    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    const SET_VAL: u16 = 2;
    let idx = match led {
        Led::Zero => 36,
        Led::One => 38,
    };
    let (code, _) =
        userlib::sys_send(gpio_driver, SET_VAL, &[idx, 1], &mut [], &[]);
    assert_eq!(code, 0);
}

#[cfg(feature = "lpc55")]
fn led_toggle(led: Led) {
    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    const SET_VAL: u16 = 2;
    const READ_VAL: u16 = 3;
    let idx = match led {
        Led::Zero => 36,
        Led::One => 38,
    };
    let mut val: u32 = 0;

    let (code, _) = userlib::sys_send(
        gpio_driver,
        READ_VAL,
        &[idx],
        val.as_bytes_mut(),
        &[],
    );
    assert_eq!(code, 0);

    if val == 1 {
        let (code, _) =
            userlib::sys_send(gpio_driver, SET_VAL, &[idx, 0], &mut [], &[]);
        assert_eq!(code, 0);
    } else {
        let (code, _) =
            userlib::sys_send(gpio_driver, SET_VAL, &[idx, 1], &mut [], &[]);
        assert_eq!(code, 0);
    }
}
