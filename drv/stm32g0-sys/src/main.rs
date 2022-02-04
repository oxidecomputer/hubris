// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32G0 RCC and GPIO blocks, combined for compactness.

#![no_std]
#![no_main]

use stm32g0 as pac;

#[cfg(feature = "g031")]
use stm32g0::stm32g031 as device;

#[cfg(feature = "g070")]
use stm32g0::stm32g070 as device;

#[cfg(feature = "g0b1")]
use stm32g0::stm32g0b1 as device;

use drv_stm32g0_sys_api::{GpioError, Group, RccError};
use drv_stm32xx_gpio_common::{server::get_gpio_regs, Port};
use idol_runtime::RequestError;
use userlib::*;

trait FlagsRegister {
    /// Sets bit `index` in the register, preserving other bits.
    ///
    /// # Safety
    ///
    /// This is unsafe because, in theory, you might be able to find a register
    /// where setting a bit can imperil memory safety. It is your responsibility
    /// not to use this on such registers.
    unsafe fn set_bit(&self, index: u8);

    /// Clears bit `index` in the register, preserving other bits.
    ///
    /// # Safety
    ///
    /// This is unsafe because, in theory, you might be able to find a register
    /// where clearing a bit can imperil memory safety. It is your
    /// responsibility not to use this on such registers.
    unsafe fn clear_bit(&self, index: u8);
}

impl<S> FlagsRegister for pac::Reg<S>
where
    S: pac::RegisterSpec<Ux = u32> + pac::Readable + pac::Writable,
{
    unsafe fn set_bit(&self, index: u8) {
        self.modify(|r, w| unsafe { w.bits(r.bits() | 1 << index) });
    }

    unsafe fn clear_bit(&self, index: u8) {
        self.modify(|r, w| unsafe { w.bits(r.bits() & !(1 << index)) });
    }
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
    fn unpack_raw(raw: u32) -> Result<(Group, u8), RequestError<RccError>> {
        let bit: u8 = (raw & 0x1F) as u8;
        let bus =
            Group::from_u32(raw >> 5).ok_or(RccError::NoSuchPeripheral)?;
        // TODO: this lets people refer to bit indices that are not included in
        // the Peripheral enum, which is not great. Fixing this by deriving
        // FromPrimitive for Peripheral results in _really expensive_ checking
        // code. We could do better.
        Ok((bus, bit))
    }
}

impl idl::InOrderSysImpl for ServerImpl<'_> {
    fn enable_clock_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        match Self::unpack_raw(raw)? {
            (Group::Iop, bit) => unsafe { self.rcc.iopenr.set_bit(bit) },
            (Group::Ahb, bit) => unsafe { self.rcc.ahbenr.set_bit(bit) },
            (Group::Apb1, bit) => unsafe { self.rcc.apbenr1.set_bit(bit) },
            (Group::Apb2, bit) => unsafe { self.rcc.apbenr2.set_bit(bit) },
        }
        Ok(())
    }

    fn disable_clock_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        match Self::unpack_raw(raw)? {
            (Group::Iop, bit) => unsafe { self.rcc.iopenr.clear_bit(bit) },
            (Group::Ahb, bit) => unsafe { self.rcc.ahbenr.clear_bit(bit) },
            (Group::Apb1, bit) => unsafe { self.rcc.apbenr1.clear_bit(bit) },
            (Group::Apb2, bit) => unsafe { self.rcc.apbenr2.clear_bit(bit) },
        }
        Ok(())
    }

    fn enter_reset_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        match Self::unpack_raw(raw)? {
            (Group::Iop, bit) => unsafe { self.rcc.ioprstr.set_bit(bit) },
            (Group::Ahb, bit) => unsafe { self.rcc.ahbrstr.set_bit(bit) },
            (Group::Apb1, bit) => unsafe { self.rcc.apbrstr1.set_bit(bit) },
            (Group::Apb2, bit) => unsafe { self.rcc.apbrstr2.set_bit(bit) },
        }
        Ok(())
    }

    fn leave_reset_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        match Self::unpack_raw(raw)? {
            (Group::Iop, bit) => unsafe { self.rcc.ioprstr.clear_bit(bit) },
            (Group::Ahb, bit) => unsafe { self.rcc.ahbrstr.clear_bit(bit) },
            (Group::Apb1, bit) => unsafe { self.rcc.apbrstr1.clear_bit(bit) },
            (Group::Apb2, bit) => unsafe { self.rcc.apbrstr2.clear_bit(bit) },
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
