//! A driver for the STM32F4 Reset and Clock Controller (RCC).
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

use stm32f4::stm32f407 as device;
use zerocopy::AsBytes;
use userlib::*;

const OP_ENABLE_CLOCK: u32 = 1;
const OP_DISABLE_CLOCK: u32 = 2;
const OP_ENTER_RESET: u32 = 3;
const OP_LEAVE_RESET: u32 = 4;

#[repr(u32)]
enum ResponseCode {
    Success = 0,
    BadOp = 1,
    BadArg = 2,
}

// None of the registers we interact with have the same types, and they share no
// useful traits, so we can't extract the bit-setting routine into a function --
// we have no choice but to use macros.
macro_rules! set_bits {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() | $mask) });
    };
}

// None of the registers we interact with have the same types, and they share no
// useful traits, so we can't extract the bit-clearing routine into a function
// -- we have no choice but to use macros.
macro_rules! clear_bits {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() & !$mask) });
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
    let mask = 0;  // we don't use notifications.
    let mut buffer = 0u32;
    loop {
        let msginfo = sys_recv(buffer.as_bytes_mut(), mask);
        let pmask = 1 << (buffer % 32);
        let chunk = buffer / 32;
        match msginfo.operation {
            // Note: you're probably looking at the match arms below and saying
            // to yourself, "gosh, I bet we could eliminate some duplication
            // here." Well, good luck. svd2rust has ensured that every *ENR and
            // *RSTR register is a *totally distinct type*, meaning we can't
            // operate on them generically.

            OP_ENABLE_CLOCK => {
                match chunk {
                    0 => {
                        // AHB1
                        set_bits!(rcc.ahb1enr, pmask);
                    }
                    1 => {
                        // AHB2
                        set_bits!(rcc.ahb2enr, pmask);
                    }
                    2 => {
                        // AHB3
                        set_bits!(rcc.ahb3enr, pmask);
                    }
                    3 => {
                        // APB1
                        set_bits!(rcc.apb1enr, pmask);
                    }
                    4 => {
                        // APB2
                        set_bits!(rcc.apb2enr, pmask);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }
                }
            }
            OP_DISABLE_CLOCK => {
                match chunk {
                    0 => {
                        // AHB1
                        clear_bits!(rcc.ahb1enr, pmask);
                    }
                    1 => {
                        // AHB2
                        clear_bits!(rcc.ahb2enr, pmask);
                    }
                    2 => {
                        // AHB3
                        clear_bits!(rcc.ahb3enr, pmask);
                    }
                    3 => {
                        // APB1
                        clear_bits!(rcc.apb1enr, pmask);
                    }
                    4 => {
                        // APB2
                        clear_bits!(rcc.apb2enr, pmask);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }
                }
            }
            OP_ENTER_RESET => {
                match chunk {
                    0 => {
                        // AHB1
                        set_bits!(rcc.ahb1rstr, pmask);
                    }
                    1 => {
                        // AHB2
                        set_bits!(rcc.ahb2rstr, pmask);
                    }
                    2 => {
                        // AHB3
                        set_bits!(rcc.ahb3rstr, pmask);
                    }
                    3 => {
                        // APB1
                        set_bits!(rcc.apb1rstr, pmask);
                    }
                    4 => {
                        // APB2
                        set_bits!(rcc.apb2rstr, pmask);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }
                }
            }
            OP_LEAVE_RESET => {
                match chunk {
                    0 => {
                        // AHB1
                        clear_bits!(rcc.ahb1rstr, pmask);
                    }
                    1 => {
                        // AHB2
                        clear_bits!(rcc.ahb2rstr, pmask);
                    }
                    2 => {
                        // AHB3
                        clear_bits!(rcc.ahb3rstr, pmask);
                    }
                    3 => {
                        // APB1
                        clear_bits!(rcc.apb1rstr, pmask);
                    }
                    4 => {
                        // APB2
                        clear_bits!(rcc.apb2rstr, pmask);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                        continue;
                    }
                }
            }
            _ => {
                // Unrecognized operation code
                sys_reply(msginfo.sender, ResponseCode::BadOp as u32, &[]);
                continue;
            }
        }

        // If we reach this point, we were successful; factor out the reply:
        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
    }
}
