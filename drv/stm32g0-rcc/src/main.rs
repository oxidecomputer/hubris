// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32G0 Reset and Clock Controller (RCC).
//!
//! This driver puts the system into a reasonable initial state, and then fields
//! requests to alter settings on behalf of other drivers. This prevents us from
//! needing to map the popular registers in the RCC into every driver task.
//!
//! # IPC protocol
//!
//! ## `enable_clock` (1)
//!
//! Requests that the clock to a peripheral be turned on.
//!
//! Peripherals are numbered by bit number in the RCC control registers, as
//! follows:
//!
//! - RCC_IOPENR[31:0] are indices 31-0.
//! - RCC_AHBENR[31:0] are indices 63-32.
//! - RCC_APBENR1[31:0] are indices 95-64.
//! - RCC_APBENR2[31:0] are indices 127-96.
//!
//! Request message format: single `u32` giving peripheral index as described
//! above.
//!
//! ## `disable_clock` (2)
//!
//! Requests that the clock to a peripheral be turned off.
//!
//! Request message format: single `u32` giving peripheral index as described
//! for `enable_clock`.
//!
//! ## `enter_reset` (3)
//!
//! Requests that the reset line to a peripheral be asserted.
//!
//! Peripherals are numbered by bit number in the RCC control registers, as
//! follows:
//!
//! - RCC_IOPRSTR[31:0] are indices 31-0.
//! - RCC_AHBRSTR[31:0] are indices 63-32.
//! - RCC_APBRSTR1[31:0] are indices 95-64.
//! - RCC_APBRSTR2[31:0] are indices 127-96.
//!
//! Request message format: single `u32` giving peripheral index as described
//! above.
//!
//! ## `leave_reset` (4)
//!
//! Requests that the reset line to a peripheral be deasserted.
//!
//! Request message format: single `u32` giving peripheral index as described
//! for `enter_reset`.

#![no_std]
#![no_main]

#[cfg(feature = "g031")]
use stm32g0::stm32g031 as device;

#[cfg(feature = "g070")]
use stm32g0::stm32g070 as device;

#[cfg(feature = "g0b1")]
use stm32g0::stm32g0b1 as device;

use idol_runtime::RequestError;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive)]
#[repr(u32)]
pub enum RccError {
    NoSuchPeripheral = 1,
}

impl From<u32> for RccError {
    fn from(x: u32) -> Self {
        match x {
            1 => RccError::NoSuchPeripheral,
            _ => panic!(),
        }
    }
}

impl From<RccError> for u16 {
    fn from(x: RccError) -> Self {
        x as u16
    }
}

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

    // Any global setup we required would go here.

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
        let pmask: u32 = 1 << (raw % 32);
        let bus = Bus::from_u32(raw / 32).ok_or(RccError::NoSuchPeripheral)?;
        Ok((bus, pmask))
    }
}

impl idl::InOrderRccImpl for ServerImpl<'_> {
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
}

mod idl {
    use super::RccError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
