// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32F3/4 Reset and Clock Controller (RCC).
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
//! - AHB1ENR[31:0] are indices 31-0.
//! - AHB2ENR[31:0] are indices 63-32.
//! - Then AHB3ENR,
//! - Then APB1ENR,
//! - Then APB2ENR.
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

#[cfg(feature = "stm32f3")]
use stm32f3::stm32f303 as device;

#[cfg(feature = "f407")]
use stm32f4::stm32f407 as device;

#[cfg(feature = "f429")]
use stm32f4::stm32f429 as device;

use userlib::*;
use zerocopy::IntoBytes;

#[derive(FromPrimitive)]
enum Op {
    EnableClock = 1,
    DisableClock = 2,
    EnterReset = 3,
    LeaveReset = 4,
}

#[derive(FromPrimitive)]
enum Bus {
    Ahb1 = 0,
    Ahb2 = 1,
    Ahb3 = 2,
    Apb1 = 3,
    Apb2 = 4,
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
    let mut buffer = [0u32; 1];
    loop {
        hl::recv_without_notification(
            buffer.as_mut_bytes(),
            |op, msg| -> Result<(), ResponseCode> {
                // Every incoming message uses the same payload type and
                // response type: it's always u32 -> (). So we can do the
                // check-and-convert here:
                let (msg, caller) =
                    msg.fixed::<u32, ()>().ok_or(ResponseCode::BadArg)?;
                let pmask: u32 = 1 << (msg % 32);
                let bus =
                    Bus::from_u32(msg / 32).ok_or(ResponseCode::BadArg)?;

                // Note: you're probably looking at the match arms below and
                // saying to yourself, "gosh, I bet we could eliminate some
                // duplication here." Well, good luck. svd2rust has ensured that
                // every *ENR and *RSTR register is a *totally distinct type*,
                // meaning we can't operate on them generically.
                //
                // STMF3 boards only have the single AHB bus, so error out
                // if any other bus is requested
                match op {
                    Op::EnableClock => match bus {
                        #[cfg(feature = "stm32f3")]
                        Bus::Ahb1 => set_bits!(rcc.ahbenr, pmask),
                        #[cfg(feature = "stm32f3")]
                        Bus::Ahb2 | Bus::Ahb3 => {
                            return Err(ResponseCode::BadArg)
                        }

                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb1 => set_bits!(rcc.ahb1enr, pmask),
                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb2 => set_bits!(rcc.ahb2enr, pmask),
                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb3 => set_bits!(rcc.ahb3enr, pmask),

                        Bus::Apb1 => set_bits!(rcc.apb1enr, pmask),
                        Bus::Apb2 => set_bits!(rcc.apb2enr, pmask),
                    },
                    Op::DisableClock => match bus {
                        #[cfg(feature = "stm32f3")]
                        Bus::Ahb1 => clear_bits!(rcc.ahbenr, pmask),
                        #[cfg(feature = "stm32f3")]
                        Bus::Ahb2 | Bus::Ahb3 => {
                            return Err(ResponseCode::BadArg)
                        }

                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb1 => clear_bits!(rcc.ahb1enr, pmask),
                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb2 => clear_bits!(rcc.ahb2enr, pmask),
                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb3 => clear_bits!(rcc.ahb3enr, pmask),

                        Bus::Apb1 => clear_bits!(rcc.apb1enr, pmask),
                        Bus::Apb2 => clear_bits!(rcc.apb2enr, pmask),
                    },
                    Op::EnterReset => match bus {
                        #[cfg(feature = "stm32f3")]
                        Bus::Ahb1 => set_bits!(rcc.ahbrstr, pmask),
                        #[cfg(feature = "stm32f3")]
                        Bus::Ahb2 | Bus::Ahb3 => {
                            return Err(ResponseCode::BadArg)
                        }

                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb1 => set_bits!(rcc.ahb1rstr, pmask),
                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb2 => set_bits!(rcc.ahb2rstr, pmask),
                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb3 => set_bits!(rcc.ahb3rstr, pmask),

                        Bus::Apb1 => set_bits!(rcc.apb1rstr, pmask),
                        Bus::Apb2 => set_bits!(rcc.apb2rstr, pmask),
                    },
                    Op::LeaveReset => match bus {
                        #[cfg(feature = "stm32f3")]
                        Bus::Ahb1 => clear_bits!(rcc.ahbrstr, pmask),
                        #[cfg(feature = "stm32f3")]
                        Bus::Ahb2 | Bus::Ahb3 => {
                            return Err(ResponseCode::BadArg)
                        }

                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb1 => clear_bits!(rcc.ahb1rstr, pmask),
                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb2 => clear_bits!(rcc.ahb2rstr, pmask),
                        #[cfg(feature = "stm32f4")]
                        Bus::Ahb3 => clear_bits!(rcc.ahb3rstr, pmask),

                        Bus::Apb1 => clear_bits!(rcc.apb1rstr, pmask),
                        Bus::Apb2 => clear_bits!(rcc.apb2rstr, pmask),
                    },
                }

                caller.reply(());
                Ok(())
            },
        );
    }
}
