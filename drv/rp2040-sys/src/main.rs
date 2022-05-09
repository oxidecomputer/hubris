// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the RP2040 systemy bits

#![no_std]
#![no_main]

use rp2040_pac as device;

use drv_rp2040_sys_api::{Resets, CantFail};
use core::convert::Infallible;
use idol_runtime::{ClientError, RequestError};
use userlib::*;

#[export_name = "main"]
fn main() -> ! {
    let resets = unsafe { &*device::RESETS::ptr() };

    // Bring some things we use out of reset.
    resets.reset.modify(|_, w| w
        .io_bank0().clear_bit()
    );

    while !resets.reset_done.read().io_bank0().bit() {}

    let sio = unsafe { &*device::SIO::ptr() };

    sio.gpio_oe_set.write(|w| unsafe { w.bits(1 << 25) });
    sio.gpio_out_set.write(|w| unsafe { w.bits(1 << 25) });

    // Field messages.
    let mut buffer = [0u8; idl::INCOMING_SIZE];
    let mut server = ServerImpl { resets, sio };
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl<'a> {
    resets: &'a device::resets::RegisterBlock,
    sio: &'a device::sio::RegisterBlock,
}

impl idl::InOrderSysImpl for ServerImpl<'_> {
    fn enter_reset_raw(
        &mut self,
        _: &RecvMessage,
        peripherals: u32,
    ) -> Result<(), RequestError<Infallible>> {
        // Refuse any reserved bits in the reset register.
        // PAC/svd2rust is no help here; easiest thing is to use our bitfield.
        Resets::from_bits(peripherals)
            .ok_or(ClientError::BadMessageContents.fail())?;

        self.resets.reset.modify(|r, w| unsafe {
            w.bits(r.bits() | peripherals)
        });
        Ok(())
    }

    fn leave_reset_raw(
        &mut self,
        _: &RecvMessage,
        peripherals: u32,
    ) -> Result<(), RequestError<Infallible>> {
        // Refuse any reserved bits in the reset register.
        // PAC/svd2rust is no help here; easiest thing is to use our bitfield.
        Resets::from_bits(peripherals)
            .ok_or(ClientError::BadMessageContents.fail())?;

        self.resets.reset.modify(|r, w| unsafe {
            w.bits(r.bits() & !peripherals)
        });
        while self.resets.reset_done.read().bits() & peripherals != 0 {
            // TODO: it's probably not great to spin forever in response to any
            // client request.
        }

        Ok(())
    }

    fn gpio_set_oe_raw(
        &mut self,
        _: &RecvMessage,
        pins: u32,
        enable: bool,
    ) -> Result<(), RequestError<Infallible>> {
        // Only pins 29:0 on this bank are implemented.
        if pins & !((1 << 29) - 1) != 0 {
            return Err(ClientError::BadMessageContents.fail());
        }

        if enable {
            self.sio.gpio_oe_set.write(|w| unsafe { w.bits(pins) });
        } else {
            self.sio.gpio_oe_clr.write(|w| unsafe { w.bits(pins) });
        }

        Ok(())
    }

    fn gpio_set_reset(
        &mut self,
        _: &RecvMessage,
        set_pins: u32,
        reset_pins: u32,
    ) -> Result<(), RequestError<Infallible>> {
        // Only pins 29:0 on this bank are implemented.
        if set_pins & !((1 << 29) - 1) != 0 {
            return Err(ClientError::BadMessageContents.fail());
        }
        if reset_pins & !((1 << 29) - 1) != 0 {
            return Err(ClientError::BadMessageContents.fail());
        }

        if set_pins != 0 {
            self.sio.gpio_out_set.write(|w| unsafe { w.bits(set_pins) });
        }
        if reset_pins != 0 {
            self.sio.gpio_out_clr.write(|w| unsafe { w.bits(reset_pins) });
        }

        Ok(())
    }

    fn gpio_toggle(
        &mut self,
        _: &RecvMessage,
        pins: u32,
    ) -> Result<(), RequestError<CantFail>> {
        // Only pins 29:0 on this bank are implemented.
        if pins & !((1 << 29) - 1) != 0 {
            return Err(ClientError::BadMessageContents.fail());
        }

        self.sio.gpio_out_xor.write(|w| unsafe { w.bits(pins) });

        Ok(())
    }
}

mod idl {
    use drv_rp2040_sys_api::CantFail;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
