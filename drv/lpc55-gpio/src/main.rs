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
use idol_runtime::{NotificationHandler, RequestError};
use userlib::{task_slot, RecvMessage};

task_slot!(SYSCON, syscon_driver);

struct ServerImpl<'a> {
    gpio: &'a device::gpio::RegisterBlock,
    pint: &'a device::pint::RegisterBlock,
    inputmux: &'a device::inputmux::RegisterBlock,
}

impl ServerImpl<'_> {
    fn set_pin_direction(&self, port: usize, pin: usize, dir: Direction) {
        match dir {
            Direction::Input => self.gpio.dirclr[port]
                .write(|w| unsafe { w.dirclrp().bits(1 << pin) }),
            Direction::Output => self.gpio.dirset[port]
                .write(|w| unsafe { w.dirsetp().bits(1 << pin) }),
        }
    }
}

impl idl::InOrderPinsImpl for ServerImpl<'_> {
    fn set_dir(
        &mut self,
        _: &RecvMessage,
        pin: Pin,
        dir: Direction,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        let (port, pin) = gpio_port_pin_validate(pin);
        self.set_pin_direction(port, pin, dir);
        Ok(())
    }

    fn set_val(
        &mut self,
        _: &RecvMessage,
        pin: Pin,
        val: Value,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        let (port, pin) = gpio_port_pin_validate(pin);

        match val {
            Value::One => self.gpio.set[port]
                .write(|w| unsafe { w.setp().bits(1 << pin) }),
            Value::Zero => self.gpio.clr[port]
                .write(|w| unsafe { w.clrp().bits(1 << pin) }),
        }

        Ok(())
    }

    fn read_val(
        &mut self,
        _: &RecvMessage,
        pin: Pin,
    ) -> Result<Value, RequestError<core::convert::Infallible>> {
        let (port, pin) = gpio_port_pin_validate(pin);

        let mask = 1 << pin;

        let val = (self.gpio.pin[port].read().port().bits() & mask) == mask;

        if val {
            Ok(Value::One)
        } else {
            Ok(Value::Zero)
        }
    }

    fn toggle(
        &mut self,
        _: &RecvMessage,
        pin: Pin,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        let (port, pin) = gpio_port_pin_validate(pin);

        self.gpio.not[port].write(|w| unsafe { w.notp().bits(1 << pin) });
        Ok(())
    }

    fn iocon_configure_raw(
        &mut self,
        _: &RecvMessage,
        pin: Pin,
        conf: u32,
        pint_slot: PintSlot,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        // The LPC55 IOCON Rust API has individual functions for each pin.
        // This is not easily compatible with our API that involves passing
        // around a representation of each pin. Given we have to pack the
        // field in order to send it anyway it's much easier just to write
        // the register manually
        let iocon_base = device::IOCON::ptr() as *const u32 as u32;

        let base = iocon_base + 4 * (pin as u32);

        unsafe {
            core::ptr::write_volatile(base as *mut u32, conf);
        }

        // If the GPIO pin is configured for interrupts, then
        // interrupt configuration is done before pin configuration.
        // INPUTMUX from UM11126:
        // Once set up, no clocks are required for the input multiplexer to
        // function. The system clock is needed only to write to or read from
        // the INPUT MUX registers. Once the input multiplexer is configured,
        // disable the clock to the INPUT MUX block in the AHBCLKCTRL register.
        // See: Section 4.5.17 “AHB clock control 0”.
        match pint_slot {
            PintSlot::None => {}
            PintSlot::Slot0
            | PintSlot::Slot1
            | PintSlot::Slot2
            | PintSlot::Slot3
            | PintSlot::Slot4
            | PintSlot::Slot5
            | PintSlot::Slot6
            | PintSlot::Slot7 => {
                // The INPUTMUX only needs to be turned on during configuration.
                let syscon = Syscon::from(SYSCON.get_task_id());
                syscon.enable_clock(Peripheral::Mux);
                syscon.leave_reset(Peripheral::Mux);
                unsafe {
                    self.inputmux.pintsel[pint_slot as usize]
                        .write(|w| w.bits(pin as u32));
                }
                syscon.disable_clock(Peripheral::Mux);

                // NOTE: We're only supporting edge-triggered interrupts right now.
                // We hard-code ISEL.PMODE as edge-triggered:
                // Edge triggered = 0 << PintSlot
                // Level triggered = 1 << PintSlot
                let mask = 1u32 << (pint_slot as usize);
                unsafe {
                    self.pint.isel.modify(|r, w| w.bits(r.bits() & !mask));
                }
            }
        }

        Ok(())
    }

    //
    // Functions for managing GPIO interrupts:
    //

    fn clear_rising(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
        self.pint.rise.write(|w| unsafe { w.bits(pint_mask) });
        Ok(())
    }

    fn clear_falling(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
        self.pint.fall.write(|w| unsafe { w.bits(pint_mask) });
        Ok(())
    }

    fn clear_status(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
        self.pint.ist.write(|w| unsafe { w.bits(pint_mask) });
        Ok(())
    }

    fn detected_rising(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<u32, idol_runtime::RequestError<core::convert::Infallible>>
    {
        let status = self.pint.rise.read().bits() & pint_mask;
        Ok(status)
    }

    fn detected_falling(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<u32, idol_runtime::RequestError<core::convert::Infallible>>
    {
        let status = self.pint.fall.read().bits() & pint_mask;
        Ok(status)
    }

    fn disable_falling(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
        self.pint.cienf.write(|w| unsafe { w.bits(pint_mask) });
        Ok(())
    }

    fn disable_rising(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
        self.pint.cienr.write(|w| unsafe { w.bits(pint_mask) });
        Ok(())
    }

    fn enable_falling(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
        // XXX sanity check pint_mask
        self.pint.sienf.write(|w| unsafe { w.bits(pint_mask) });
        Ok(())
    }

    fn enable_rising(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
        self.pint.sienr.write(|w| unsafe { w.bits(pint_mask) });
        Ok(())
    }

    fn read_status(
        &mut self,
        _: &RecvMessage,
        pint_mask: u32,
    ) -> Result<u32, idol_runtime::RequestError<core::convert::Infallible>>
    {
        let status = self.pint.ist.read().bits() & pint_mask;
        Ok(status)
    }
}

impl NotificationHandler for ServerImpl<'_> {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    turn_on_gpio_clocks();

    let gpio = unsafe { &*device::GPIO::ptr() };
    let pint = unsafe { &*device::PINT::ptr() };
    let inputmux = unsafe { &*device::INPUTMUX::ptr() };

    let mut server = ServerImpl {
        gpio,
        pint,
        inputmux,
    };

    let mut incoming = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

fn gpio_port_pin_validate(pin: Pin) -> (usize, usize) {
    let _pin = pin as u32;

    // These are encoded such that port 0 goes to 31 and port 1 goes
    // 32 to 63
    let port = (_pin >> 5) as usize;
    let pnum = (_pin & 0b1_1111) as usize;

    (port, pnum)
}

fn turn_on_gpio_clocks() {
    let syscon = Syscon::from(SYSCON.get_task_id());

    syscon.enable_clock(Peripheral::Iocon);
    syscon.leave_reset(Peripheral::Iocon);

    syscon.enable_clock(Peripheral::Gpio0);
    syscon.leave_reset(Peripheral::Gpio0);

    syscon.enable_clock(Peripheral::Gpio1);
    syscon.leave_reset(Peripheral::Gpio1);

    syscon.enable_clock(Peripheral::Pint);
    syscon.leave_reset(Peripheral::Pint);
}

mod idl {
    use crate::PintSlot;
    use drv_lpc55_gpio_api::{Direction, Pin, Value};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
