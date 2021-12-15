// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55S6x GPIO
//!
//! GPIO is covered by two separate hardware blocks: GPIO which handles the
//! typical GPIO set low/set high and IOCON which handles the pin configuration.
//!
//! This driver depends on the SYSCON driver being available
//!
//! GPIOs are specified via PIO{0,1}_{0-31}. For these APIs the numbers are,
//! PIO0_{n} = n
//! PIO1_{n} = 32 + n
//!
//! # IPC protocol
//!
//! ## `set_dir` (1)
//!
//! Sets the direction of the corresponding GPIO number, 0 = input, 1 = output
//!
//! Request message format: two `u8` giving GPIO number and direction
//!
//! ## `set_val` (2)
//!
//! Sets the digital value (0 or 1) to the corresponding GPIO number. The
//! GPIO pin must be configured as GPIO and an output already.
//!
//! Request message format: two `u8` giving GPIO number and value
//!
//! ## `read_val` (3)
//!
//! Reads the digital value to the corresponding GPIO number. The GPIO
//! pin must be configured as GPIO and an input already.
//!
//! Request message format: single `u8` giving GPIO number
//! Returns: Digital value
//!

#![no_std]
#![no_main]

use lpc55_pac as device;

use drv_lpc55_gpio_api::*;
use drv_lpc55_syscon_api::*;
use hl;
use userlib::{FromPrimitive, *};

task_slot!(SYSCON, syscon_driver);

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

// Generates a gigantic match table for each pin
lpc55_iocon_gen::gen_iocon_table!();

#[export_name = "main"]
fn main() -> ! {
    turn_on_gpio_clocks();

    let gpio = unsafe { &*device::GPIO::ptr() };

    // Handler for received messages.
    let recv_handler = |op: Op, msg: hl::Message| -> Result<(), ResponseCode> {
        match op {
            Op::SetDir => {
                let (msg, caller) = msg
                    .fixed::<DirectionRequest, ()>()
                    .ok_or(ResponseCode::BadArg)?;
                let dir =
                    Direction::from_u32(msg.dir).ok_or(ResponseCode::BadArg)?;

                let (port, pin) = gpio_port_pin_validate(msg.pin)?;

                match dir {
                    Direction::Input => gpio.dirclr[port]
                        .write(|w| unsafe { w.dirclrp().bits(1 << pin) }),
                    Direction::Output => gpio.dirset[port]
                        .write(|w| unsafe { w.dirsetp().bits(1 << pin) }),
                }
                caller.reply(());
                Ok(())
            }
            Op::SetVal => {
                let (msg, caller) = msg
                    .fixed::<SetRequest, ()>()
                    .ok_or(ResponseCode::BadArg)?;

                let (port, pin) = gpio_port_pin_validate(msg.pin)?;

                let val =
                    Value::from_u32(msg.val).ok_or(ResponseCode::BadArg)?;

                match val {
                    Value::One => gpio.set[port]
                        .write(|w| unsafe { w.setp().bits(1 << pin) }),
                    Value::Zero => gpio.clr[port]
                        .write(|w| unsafe { w.clrp().bits(1 << pin) }),
                }
                caller.reply(());
                Ok(())
            }
            Op::ReadVal => {
                // Make sure the pin is set in digital mode before trying to
                // use this function otherwise it will not work!
                let (msg, caller) = msg
                    .fixed::<ReadRequest, u8>()
                    .ok_or(ResponseCode::BadArg)?;

                let (port, pin) = gpio_port_pin_validate(msg.pin)?;

                let mask = 1 << pin;

                let val = (gpio.pin[port].read().port().bits() & mask) == mask;
                caller.reply(val as u8);
                Ok(())
            }
            Op::Toggle => {
                let (msg, caller) = msg
                    .fixed::<ToggleRequest, ()>()
                    .ok_or(ResponseCode::BadArg)?;

                let (port, pin) = gpio_port_pin_validate(msg.pin)?;

                gpio.not[port].write(|w| unsafe { w.notp().bits(1 << pin) });

                caller.reply(());
                Ok(())
            }
            Op::Configure => {
                let (msg, caller) = msg
                    .fixed::<ConfigureRequest, ()>()
                    .ok_or(ResponseCode::BadArg)?;

                let conf = msg.conf;

                let pin = Pin::from_u32(msg.pin).ok_or(ResponseCode::BadArg)?;

                let func = AltFn::from_u32(conf & 0b1111)
                    .ok_or(ResponseCode::BadArg)?;
                let mode = Mode::from_u32((conf >> 4) & 0b11)
                    .ok_or(ResponseCode::BadArg)?;
                let slew = Slew::from_u32((conf >> 6) & 1)
                    .ok_or(ResponseCode::BadArg)?;
                let invert = Invert::from_u32((conf >> 7) & 1)
                    .ok_or(ResponseCode::BadArg)?;
                let digimode = Digimode::from_u32((conf >> 8) & 1)
                    .ok_or(ResponseCode::BadArg)?;
                let opendrain = Opendrain::from_u32((conf >> 9) & 1)
                    .ok_or(ResponseCode::BadArg)?;

                set_iocon(pin, func, mode, slew, invert, digimode, opendrain);

                caller.reply(());
                Ok(())
            }
        }
    };

    // Field messages.
    let mut buffer: [u8; 12] = [0; 12];
    loop {
        hl::recv_without_notification(&mut buffer, recv_handler);
    }
}

fn gpio_port_pin_validate(pin: u32) -> Result<(usize, usize), ResponseCode> {
    let _ = Pin::from_u32(pin).ok_or(ResponseCode::BadArg)?;

    // These are encoded such that port 0 goes to 31 and port 1 goes
    // 32 to 63
    let port = (pin >> 5) as usize;
    let pnum = (pin & 0b1_1111) as usize;

    Ok((port, pnum))
}

fn turn_on_gpio_clocks() {
    let syscon = Syscon::from(SYSCON.get_task_id());

    syscon.enable_clock(Peripheral::Iocon);
    syscon.leave_reset(Peripheral::Iocon);

    syscon.enable_clock(Peripheral::Gpio0);
    syscon.leave_reset(Peripheral::Gpio0);

    syscon.enable_clock(Peripheral::Gpio1);
    syscon.leave_reset(Peripheral::Gpio1);
}
