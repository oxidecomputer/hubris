// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32L0 RCC and GPIO blocks, combined for compactness.

#![no_std]
#![no_main]

#[cfg(feature = "l0x3")]
use stm32l0::stm32l0x3 as device;

use drv_stm32l0_sys_api::{GpioError, Port, RccError};
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
            .iopeen()
            .set_bit()
            .iophen()
            .set_bit()
    });

    // Field messages.
    // Ensure our buffer is aligned properly for a u32 by declaring it as one.
    let mut buffer = [0u8; idl::INCOMING_SIZE];
    let mut server = ServerImpl { rcc };
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

// These macros are required as the different GPIO ports return different types
// for some reason (GPIOA returns gpioa, GPIO[B-F] return gpiob?!).
macro_rules! do_configure {
    ($reg:expr, $pins:expr, $packed_attributes:expr) => {{
        // The GPIO config registers come in 1, 2, and 4-bit per
        // field variants. The user-submitted mask is already
        // correct for the 1-bit fields; we need to expand it
        // into corresponding 2- and 4-bit masks. We use an
        // outer perfect shuffle operation for this, which
        // interleaves zeroes from the top 16 bits into the
        // bottom 16.

        // 1 in each targeted 1bit field.
        let mask_1 = u32::from($pins);
        // 0b01 in each targeted 2bit field.
        let lsbs_2 = outer_perfect_shuffle(mask_1);
        // 0b0001 in each targeted 4bit field for low half.
        let lsbs_4l = outer_perfect_shuffle(lsbs_2 & 0xFFFF);
        // Same for high half.
        let lsbs_4h = outer_perfect_shuffle(lsbs_2 >> 16);

        // Corresponding masks, with 1s in all field bits
        // instead of just the LSB:
        let mask_2 = lsbs_2 * 0b11;
        let mask_4l = lsbs_4l * 0b1111;
        let mask_4h = lsbs_4h * 0b1111;

        let atts = $packed_attributes;

        // MODER contains 16x 2-bit fields.
        let moder_val = u32::from(atts & 0b11);
        $reg.moder.write(|w| unsafe {
            w.bits(($reg.moder.read().bits() & !mask_2) | (moder_val * lsbs_2))
        });
        // OTYPER contains 16x 1-bit fields.
        let otyper_val = u32::from((atts >> 2) & 1);
        $reg.otyper.write(|w| unsafe {
            w.bits(
                ($reg.otyper.read().bits() & !mask_1) | (otyper_val * mask_1),
            )
        });
        // OSPEEDR contains 16x 2-bit fields.
        let ospeedr_val = u32::from((atts >> 3) & 0b11);
        $reg.ospeedr.write(|w| unsafe {
            w.bits(
                ($reg.ospeedr.read().bits() & !mask_2) | (ospeedr_val * lsbs_2),
            )
        });
        // PUPDR contains 16x 2-bit fields.
        let pupdr_val = u32::from((atts >> 5) & 0b11);
        $reg.pupdr.write(|w| unsafe {
            w.bits(($reg.pupdr.read().bits() & !mask_2) | (pupdr_val * lsbs_2))
        });
        // AFRx contains 8x 4-bit fields.
        let af_val = u32::from((atts >> 7) & 0b1111);
        $reg.afrl.write(|w| unsafe {
            w.bits(($reg.afrl.read().bits() & !mask_4l) | (af_val * lsbs_4l))
        });
        $reg.afrh.write(|w| unsafe {
            w.bits(($reg.afrh.read().bits() & !mask_4h) | (af_val * lsbs_4h))
        });
    }};
}

macro_rules! do_set_reset {
    ($reg:expr, $set:expr, $reset:expr) => {
        $reg.bsrr.write(|w| unsafe {
            w.bits((u32::from($reset) << 16) | u32::from($set))
        })
    };
}

macro_rules! do_toggle {
    ($reg:expr, $pins:expr) => {{
        // Read current pin *output* states.
        let state = $reg.odr.read().bits();
        // Compute BSRR value to toggle all pins. That is, move
        // currently set bits into reset position, and set unset
        // bits.
        let bsrr_all = state << 16 | state ^ 0xFFFF;
        // Write that value, but masked as the user requested.
        let bsrr_mask = u32::from($pins) * 0x1_0001;
        $reg.bsrr.write(|w| unsafe { w.bits(bsrr_all & bsrr_mask) });
    }};
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

impl idl::InOrderSysImpl for ServerImpl<'_> {
    fn enable_clock_raw(
        &mut self,
        _: &RecvMessage,
        raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        match Self::unpack_raw(raw)? {
            (Bus::Iop, pmask) => set_bits!(self.rcc.iopenr, pmask),
            (Bus::Ahb, pmask) => set_bits!(self.rcc.ahbenr, pmask),
            (Bus::Apb1, pmask) => set_bits!(self.rcc.apb1enr, pmask),
            (Bus::Apb2, pmask) => set_bits!(self.rcc.apb2enr, pmask),
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
            (Bus::Apb1, pmask) => clear_bits!(self.rcc.apb1enr, pmask),
            (Bus::Apb2, pmask) => clear_bits!(self.rcc.apb2enr, pmask),
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
            (Bus::Apb1, pmask) => set_bits!(self.rcc.apb1rstr, pmask),
            (Bus::Apb2, pmask) => set_bits!(self.rcc.apb2rstr, pmask),
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
            (Bus::Apb1, pmask) => clear_bits!(self.rcc.apb1rstr, pmask),
            (Bus::Apb2, pmask) => clear_bits!(self.rcc.apb2rstr, pmask),
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
        match port {
            Port::A => {
                let gpio = unsafe { &*device::GPIOA::ptr() };
                do_configure!(gpio, pins, packed_attributes);
            }
            _ => {
                let gpio = match port {
                    Port::B => unsafe { &*device::GPIOB::ptr() },
                    Port::C => unsafe { &*device::GPIOB::ptr() },
                    Port::D => unsafe { &*device::GPIOB::ptr() },
                    Port::E => unsafe { &*device::GPIOB::ptr() },
                    Port::H => unsafe { &*device::GPIOB::ptr() },
                    _ => unreachable!(),
                };
                do_configure!(gpio, pins, packed_attributes);
            }
        }
        Ok(())
    }

    fn gpio_set_reset(
        &mut self,
        _: &RecvMessage,
        port: Port,
        set_pins: u16,
        reset_pins: u16,
    ) -> Result<(), RequestError<GpioError>> {
        match port {
            Port::A => {
                let gpio = unsafe { &*device::GPIOA::ptr() };
                do_set_reset!(gpio, set_pins, reset_pins);
            }
            _ => {
                let gpio = match port {
                    Port::B => unsafe { &*device::GPIOB::ptr() },
                    Port::C => unsafe { &*device::GPIOB::ptr() },
                    Port::D => unsafe { &*device::GPIOB::ptr() },
                    Port::E => unsafe { &*device::GPIOB::ptr() },
                    Port::H => unsafe { &*device::GPIOB::ptr() },
                    _ => unreachable!(),
                };
                do_set_reset!(gpio, set_pins, reset_pins);
            }
        }
        Ok(())
    }

    fn gpio_toggle(
        &mut self,
        _: &RecvMessage,
        port: Port,
        pins: u16,
    ) -> Result<(), RequestError<GpioError>> {
        match port {
            Port::A => {
                let gpio = unsafe { &*device::GPIOA::ptr() };
                do_toggle!(gpio, pins);
            }
            _ => {
                let gpio = match port {
                    Port::B => unsafe { &*device::GPIOB::ptr() },
                    Port::C => unsafe { &*device::GPIOB::ptr() },
                    Port::D => unsafe { &*device::GPIOB::ptr() },
                    Port::E => unsafe { &*device::GPIOB::ptr() },
                    Port::H => unsafe { &*device::GPIOB::ptr() },
                    _ => unreachable!(),
                };
                do_toggle!(gpio, pins);
            }
        }
        Ok(())
    }

    fn gpio_read_input(
        &mut self,
        _: &RecvMessage,
        port: Port,
    ) -> Result<u16, RequestError<GpioError>> {
        match port {
            Port::A => {
                let gpio = unsafe { &*device::GPIOA::ptr() };
                Ok(gpio.idr.read().bits() as u16)
            }
            _ => {
                let gpio = match port {
                    Port::B => unsafe { &*device::GPIOB::ptr() },
                    Port::C => unsafe { &*device::GPIOB::ptr() },
                    Port::D => unsafe { &*device::GPIOB::ptr() },
                    Port::E => unsafe { &*device::GPIOB::ptr() },
                    Port::H => unsafe { &*device::GPIOB::ptr() },
                    _ => unreachable!(),
                };
                Ok(gpio.idr.read().bits() as u16)
            }
        }
    }
}

/// Interleaves bits in `input` as follows:
///
/// - Output bit 0 = input bit 0
/// - Output bit 1 = input bit 15
/// - Output bit 2 = input bit 1
/// - Output bit 3 = input bit 16
/// ...and so forth.
///
/// This is a great example of one of those bit twiddling tricks you never
/// expected to need. Method from Hacker's Delight.
const fn outer_perfect_shuffle(mut input: u32) -> u32 {
    let mut tmp = (input ^ (input >> 8)) & 0x0000ff00;
    input ^= tmp ^ (tmp << 8);
    tmp = (input ^ (input >> 4)) & 0x00f000f0;
    input ^= tmp ^ (tmp << 4);
    tmp = (input ^ (input >> 2)) & 0x0c0c0c0c;
    input ^= tmp ^ (tmp << 2);
    tmp = (input ^ (input >> 1)) & 0x22222222;
    input ^= tmp ^ (tmp << 1);
    input
}
mod idl {
    use super::{GpioError, Port, RccError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
