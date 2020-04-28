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

// you can put a breakpoint on `rust_begin_unwind` to catch panics
extern crate panic_halt;

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
                        rcc.ahb1enr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    1 => {
                        // AHB2
                        rcc.ahb2enr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    2 => {
                        // AHB3
                        rcc.ahb3enr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    3 => {
                        // APB1
                        rcc.apb1enr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    4 => {
                        // APB2
                        rcc.apb2enr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                    }
                }
            }
            OP_DISABLE_CLOCK => {
                match chunk {
                    0 => {
                        // AHB1
                        rcc.ahb1enr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    1 => {
                        // AHB2
                        rcc.ahb2enr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    2 => {
                        // AHB3
                        rcc.ahb3enr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    3 => {
                        // APB1
                        rcc.apb1enr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    4 => {
                        // APB2
                        rcc.apb2enr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                    }
                }
            }
            OP_ENTER_RESET => {
                match chunk {
                    0 => {
                        // AHB1
                        rcc.ahb1rstr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    1 => {
                        // AHB2
                        rcc.ahb2rstr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    2 => {
                        // AHB3
                        rcc.ahb3rstr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    3 => {
                        // APB1
                        rcc.apb1rstr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    4 => {
                        // APB2
                        rcc.apb2rstr.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                    }
                }
            }
            OP_LEAVE_RESET => {
                match chunk {
                    0 => {
                        // AHB1
                        rcc.ahb1rstr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    1 => {
                        // AHB2
                        rcc.ahb2rstr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    2 => {
                        // AHB3
                        rcc.ahb3rstr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    3 => {
                        // APB1
                        rcc.apb1rstr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    4 => {
                        // APB2
                        rcc.apb2rstr.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                    }
                }
            }
            _ => {
                // Unrecognized operation code
                sys_reply(msginfo.sender, ResponseCode::BadOp as u32, &[]);
            }
        }
    }
}
