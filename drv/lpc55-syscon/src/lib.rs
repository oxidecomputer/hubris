// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55S6x SYSCON block
//!
//! This driver is responsible for clocks (peripherals and PLLs), systick
//! callibration, memory remapping, id registers. Most drivers will be
//! interested in the clock bits.
//!
//! # IPC protocol
//!
//! Peripheral bit numbers per the LPC55 manual section 4.5 (for the benefit of
//! the author writing this driver who hates having to look these up. Double
//! check these later!)
//!
//! ROM = 1
//! SRAM_CTRL1 = 3
//! SRAM_CTRL2 = 4
//! SRAM_CTRL3 = 5
//! SRAM_CTRL4 = 6
//! FLASH = 7
//! FMC = 8
//! MUX = 11
//! IOCON = 13
//! GPIO0 = 14
//! GPIO1 = 15
//! PINT = 18
//! GINT = 19
//! DMA0 = 20
//! CRCGEN = 21
//! WWDT = 22
//! RTC = 23
//! MAILBOX = 26
//! ADC = 27
//! MRT = 32 + 0 = 32
//! OSTIMER = 32 + 1 = 33
//! SCT = 32 + 2 = 34
//! UTICK = 32 + 10 = 42
//! FC0 = 32 + 11 = 43
//! FC1 = 32 + 12 = 44
//! FC2 = 32 + 13 = 45
//! FC3 = 32 + 14 = 46
//! FC4 = 32 + 15 = 47
//! FC5 = 32 + 16 = 48
//! FC6 = 32 + 17 = 49
//! FC7 = 32 + 18 = 50
//! TIMER2 = 32 + 22 = 54
//! USB0_DEV = 32 + 25 = 57
//! TIMER0 = 32 + 26 = 58
//! TIMER1 = 32 + 27 = 59
//! DMA1 = 32 + 32 + 1 = 65
//! COMP = 32 + 32 + 2 = 66
//! SDIO = 32 + 32 + 3 = 67
//! USB1_HOST = 32 + 32 + 4 = 68
//! USB1_DEV = 32 + 32 + 5 = 69
//! USB1_RAM = 32 + 32 + 6 = 70
//! USB1_PHY = 32 + 32 + 7 = 71
//! FREQME = 32 + 32 + 8 = 72
//! RNG = 32 + 32 + 13 = 77
//! SYSCTL =  32 + 32 + 15 = 79
//! USB0_HOSTM = 32 + 32 + 16 = 80
//! USB0_HOSTS = 32 + 32 + 17 = 81
//! HASH_AES = 32 + 32 + 18 = 82
//! PQ = 32 + 32 + 19 = 83
//! PLULUT = 32 + 32 + 20 = 84
//! TIMER3 = 32 + 32 + 21 = 85
//! TIMER4 = 32 + 32 + 22 = 86
//! PUF = 32 + 32 + 23 = 87
//! CASPER = 32 + 32 + 24 = 88
//! ANALOG_CTRL = 32 + 32 + 27 = 91
//! HS_LSPI = 32 + 32 + 28 = 92
//! GPIO_SEC = 32 + 32 + 29 = 93
//! GPIO_SEC_INT = 32 + 32 + 30 = 94
//!
//! ## `enable_clock` (1)
//!
//! Requests that the clock to a peripheral be turned on.
//!
//! Peripherals are numbered by bit number in the SYSCON registers
//!
//! - PRESETCTRL0[31:0] are indices 31-0.
//! - PRESETCTRL1[31:0] are indices 63-32.
//! - PRESETCTRL2[31:0] are indices 64-96.
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
//! Request message format: single `u32` giving peripheral index as described
//! for `enable_clock`.
//!
//! ## `leave_reset` (4)
//!
//! Requests that the reset line to a peripheral be deasserted.
//!
//! Request message format: single `u32` giving peripheral index as described
//! for `enable_clock`.

#![no_std]
#![no_main]

use lpc55_pac as device;
use zerocopy::AsBytes;

use userlib::*;

#[derive(FromPrimitive)]
enum Op {
    EnableClock = 1,
    DisableClock = 2,
    EnterReset = 3,
    LeaveReset = 4,
}

#[derive(FromPrimitive)]
enum Reg {
    R0 = 0,
    R1 = 1,
    R2 = 2,
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

macro_rules! set_bit {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() | $mask) })
    };
}

macro_rules! clear_bit {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() & !$mask) })
    };
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = unsafe { &*device::SYSCON::ptr() };

    // Just set our Flexcom0 i.e. UART0 to be 12Mhz
    syscon.fcclksel0().modify(|_, w| w.sel().enum_0x2());
    // Flexcom4 (the DAC i2c) is also set to 12Mhz
    syscon.fcclksel4().modify(|_, w| w.sel().enum_0x2());
    // The high speed SPI AKA Flexcomm8 is also set to 12Mhz
    // Note this can definitely go higher but that involves
    // turning on PLLs and such
    syscon.hslspiclksel.modify(|_, w| w.sel().enum_0x2());

    // Field messages.
    // Ensure our buffer is aligned properly for a u32 by declaring it as one.
    let mut buffer = [0u32; 1];
    loop {
        hl::recv_without_notification(
            buffer.as_bytes_mut(),
            |op, msg| -> Result<(), ResponseCode> {
                // Every incoming message uses the same payload type and
                // response type: it's always u32 -> (). So we can do the
                // check-and-convert here:
                let (msg, caller) =
                    msg.fixed::<u32, ()>().ok_or(ResponseCode::BadArg)?;
                let pmask = 1 << (msg % 32);
                let chunk = msg / 32;

                let reg = Reg::from_u32(chunk).ok_or(ResponseCode::BadArg)?;

                // Just like the STM32F4 we end up with a lot of duplication
                // because each register is a different type.
                match op {
                    Op::EnableClock => match reg {
                        Reg::R0 => set_bit!(syscon.ahbclkctrl0, pmask),
                        Reg::R1 => set_bit!(syscon.ahbclkctrl1, pmask),
                        Reg::R2 => set_bit!(syscon.ahbclkctrl2, pmask),
                    },
                    Op::DisableClock => match reg {
                        Reg::R0 => clear_bit!(syscon.ahbclkctrl0, pmask),
                        Reg::R1 => clear_bit!(syscon.ahbclkctrl1, pmask),
                        Reg::R2 => clear_bit!(syscon.ahbclkctrl2, pmask),
                    },
                    Op::EnterReset => match reg {
                        Reg::R0 => set_bit!(syscon.presetctrl0, pmask),
                        Reg::R1 => set_bit!(syscon.presetctrl1, pmask),
                        Reg::R2 => set_bit!(syscon.presetctrl2, pmask),
                    },
                    Op::LeaveReset => match reg {
                        Reg::R0 => clear_bit!(syscon.presetctrl0, pmask),
                        Reg::R1 => clear_bit!(syscon.presetctrl1, pmask),
                        Reg::R2 => clear_bit!(syscon.presetctrl2, pmask),
                    },
                }

                caller.reply(());
                Ok(())
            },
        );
    }
}
