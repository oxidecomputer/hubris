// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32G0 RCC and GPIO blocks, combined for compactness.

#![no_std]
#![no_main]

#[cfg(feature = "g031")]
use stm32g0::stm32g031 as device;

#[cfg(feature = "g070")]
use stm32g0::stm32g070 as device;

#[cfg(feature = "g0b1")]
use stm32g0::stm32g0b1 as device;

use drv_stm32g0_sys_api::{GpioError, RccError};
use drv_stm32xx_gpio_common::{server::get_gpio_regs, Port};
use idol_runtime::RequestError;
use userlib::*;

#[derive(FromPrimitive)]
enum Bus {
    Iop = 0,
    Ahb = 1,
    Apb1 = 2,
    Apb2 = 3,
}

// None of the registers we interact with have the same types, and they share no
// useful traits, so we can't extract the bit-setting routine into a function --
// we have no choice but to use macros.
macro_rules! set_bits {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() | $mask) })
    };
}

// None of the registers we interact with have the same types, and they share no
// useful traits, so we can't extract the bit-clearing routine into a function
// -- we have no choice but to use macros.
macro_rules! clear_bits {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() & !$mask) })
    };
}

#[export_name = "main"]
fn main() -> ! {
    // From thin air, pluck a pointer to the RCC register block.
    //
    // Safety: this is needlessly unsafe in the API. The RCC is essentially a
    // static, and we access it through a & reference so aliasing is not a
    // concern. Were it literally a static, we could just reference it.
    let rcc = unsafe { &*device::RCC::ptr() };

    // Global setup.
    rcc.iopenr.write(|w| {
        w.iopaen()
            .set_bit()
            .iopben()
            .set_bit()
            .iopcen()
            .set_bit()
            .iopden()
            .set_bit()
            .iopfen()
            .set_bit();
        #[cfg(feature = "g0b1")]
        w.iopeen().set_bit();
        w
    });

    // Field messages.
    // Ensure our buffer is aligned properly for a u32 by declaring it as one.
    let mut buffer = [0u8; idl::INCOMING_SIZE];
    let mut server = ServerImpl { rcc };
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl<'a> {
    rcc: &'a device::rcc::RegisterBlock,
}

impl ServerImpl<'_> {
    fn unpack_raw(raw: u32) -> Result<(Bus, u32), RequestError<RccError>> {
        let pmask: u32 = 1 << (raw & 0x1F);
        let bus = Bus::from_u32(raw >> 5).ok_or(RccError::NoSuchPeripheral)?;
        Ok((bus, pmask))
    }
}

impl idl::InOrderSysImpl for ServerImpl<'_> {
    fn enable_clock_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        match Self::unpack_raw(raw)? {
            (Bus::Iop, pmask) => set_bits!(self.rcc.iopenr, pmask),
            (Bus::Ahb, pmask) => set_bits!(self.rcc.ahbenr, pmask),
            (Bus::Apb1, pmask) => set_bits!(self.rcc.apbenr1, pmask),
            (Bus::Apb2, pmask) => set_bits!(self.rcc.apbenr2, pmask),
        }
        Ok(())
    }

    fn disable_clock_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        match Self::unpack_raw(raw)? {
            (Bus::Iop, pmask) => clear_bits!(self.rcc.iopenr, pmask),
            (Bus::Ahb, pmask) => clear_bits!(self.rcc.ahbenr, pmask),
            (Bus::Apb1, pmask) => clear_bits!(self.rcc.apbenr1, pmask),
            (Bus::Apb2, pmask) => clear_bits!(self.rcc.apbenr2, pmask),
        }
        Ok(())
    }

    fn enter_reset_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        match Self::unpack_raw(raw)? {
            (Bus::Iop, pmask) => set_bits!(self.rcc.ioprstr, pmask),
            (Bus::Ahb, pmask) => set_bits!(self.rcc.ahbrstr, pmask),
            (Bus::Apb1, pmask) => set_bits!(self.rcc.apbrstr1, pmask),
            (Bus::Apb2, pmask) => set_bits!(self.rcc.apbrstr2, pmask),
        }
        Ok(())
    }

    fn leave_reset_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        match Self::unpack_raw(raw)? {
            (Bus::Iop, pmask) => clear_bits!(self.rcc.ioprstr, pmask),
            (Bus::Ahb, pmask) => clear_bits!(self.rcc.ahbrstr, pmask),
            (Bus::Apb1, pmask) => clear_bits!(self.rcc.apbrstr1, pmask),
            (Bus::Apb2, pmask) => clear_bits!(self.rcc.apbrstr2, pmask),
        }
        Ok(())
    }

    fn gpio_configure_raw(
        &mut self,
        _: &RecvMessage,
        port: Port,
        pins: u16,
        packed_attributes: u16,
    ) -> Result<(), RequestError<GpioError>> {
        unsafe { get_gpio_regs(port) }.configure(pins, packed_attributes);
        Ok(())
    }

    fn gpio_set_reset(
        &mut self,
        _: &RecvMessage,
        port: Port,
        set_pins: u16,
        reset_pins: u16,
    ) -> Result<(), RequestError<GpioError>> {
        unsafe { get_gpio_regs(port) }.set_reset(set_pins, reset_pins);
        Ok(())
    }

    fn gpio_toggle(
        &mut self,
        _: &RecvMessage,
        port: Port,
        pins: u16,
    ) -> Result<(), RequestError<GpioError>> {
        unsafe { get_gpio_regs(port) }.toggle(pins);
        Ok(())
    }

    fn gpio_read_input(
        &mut self,
        _: &RecvMessage,
        port: Port,
    ) -> Result<u16, RequestError<GpioError>> {
        Ok(unsafe { get_gpio_regs(port) }.read())
    }
}

mod idl {
    use super::{GpioError, Port, RccError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
