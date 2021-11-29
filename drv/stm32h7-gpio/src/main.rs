// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! STM32H7 GPIO Server.
//!
//! # IPC protocol
//!
//! ## `configure` (1)
//!
//! Applies a configuration to a subset of pins on a single port. The
//! configuration affects each of the five GPIO config registers.
//!
//! The message to this server has this format:
//!
//! ```ignore
//! #[repr(C, packed)]
//! struct ConfigureRequest {
//!     port: u8,
//!     pins: u16,
//!     packed_attributes: u16,
//! }
//! ```
//!
//! ...where `packed_attributes` bits are assigned as follows:
//!
//! - Bits 1:0: mode
//! - Bit 2: output type
//! - Bits 4:3: speed
//! - Bits 6:5: pull up/down
//! - Bits 10:7: alternate function index
//!
//! ## `set_reset` (2)
//!
//! Sets any combination of pins to either high or low, leaving others
//! unchanged.
//!
//! The message to this server has this format:
//!
//! ```ignore
//! #[repr(C, packed)]
//! struct SetResetRequest {
//!     port: u8,
//!     set_pins: u16,
//!     reset_pins: u16,
//! }
//! ```
//!
//! ## `read_input` (3)
//!
//! Reads the state of all pins on a port.
//!
//! The message to this server has this format:
//!
//! ```ignore
//! #[repr(C, packed)]
//! struct ReadInputRequest {
//!     port: u8,
//! }
//! ```
//!
//! It returns a `u16` containing the pin status.
//!
//! ## `toggle` (4)
//!
//! Toggles any combination of pins in one port, leaving others unchanged.
//!
//! The message to this server has this format:
//!
//! ```ignore
//! #[repr(C, packed)]
//! struct ToggleRequest {
//!     port: u8,
//!     pins: u16,
//! }
//! ```
//!

#![no_std]
#![no_main]

use byteorder::LittleEndian;
use drv_stm32h7_rcc_api::{Peripheral, Rcc};
use userlib::*;
use zerocopy::{AsBytes, FromBytes, Unaligned, U16, U32};

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[derive(FromPrimitive)]
enum Op {
    Configure = 1,
    SetReset = 2,
    ReadInput = 3,
    Toggle = 4,
}

#[derive(FromPrimitive)]
enum Port {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
    E = 4,
    F = 5,
    G = 6,
    H = 7,
    I = 8,
    J = 9,
    K = 10,
}

#[derive(FromBytes, Unaligned)]
#[repr(C)]
struct ConfigureRequest {
    port: u8,
    pins: U16<LittleEndian>,
    packed_attributes: U16<LittleEndian>,
}

#[derive(FromBytes, Unaligned)]
#[repr(C)]
struct SetResetRequest {
    port: u8,
    set_reset: U32<LittleEndian>,
}

#[derive(FromBytes, Unaligned)]
#[repr(C)]
struct ToggleRequest {
    port: u8,
    pins: U16<LittleEndian>,
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

task_slot!(RCC, rcc_driver);

#[export_name = "main"]
fn main() -> ! {
    // Kick things off by ensuring that all the GPIO blocks we manage are
    // powered on. Some number of blocks are powered on by the boot code; this
    // won't change that.
    turn_on_all_gpios();

    // Field messages. Our largest incoming message is 5 bytes.
    let mut buffer = [0u8; 5];
    loop {
        hl::recv_without_notification(
            buffer.as_bytes_mut(),
            |op, msg| -> Result<(), ResponseCode> {
                match op {
                    Op::Configure => {
                        let (msg, caller) = msg
                            .fixed::<ConfigureRequest, ()>()
                            .ok_or(ResponseCode::BadArg)?;
                        let port = Port::from_u8(msg.port)
                            .ok_or(ResponseCode::BadArg)?;
                        let reg = gpio_reg(port);

                        // The GPIO config registers come in 1, 2, and 4-bit per
                        // field variants. The user-submitted mask is already
                        // correct for the 1-bit fields; we need to expand it
                        // into corresponding 2- and 4-bit masks. We use an
                        // outer perfect shuffle operation for this, which
                        // interleaves zeroes from the top 16 bits into the
                        // bottom 16.

                        // 1 in each targeted 1bit field.
                        let mask_1 = u32::from(msg.pins.get());
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

                        let atts = msg.packed_attributes.get();

                        // MODER contains 16x 2-bit fields.
                        let moder_val = u32::from(atts & 0b11);
                        reg.moder.write(|w| unsafe {
                            w.bits(
                                (reg.moder.read().bits() & !mask_2)
                                    | (moder_val * lsbs_2),
                            )
                        });
                        // OTYPER contains 16x 1-bit fields.
                        let otyper_val = u32::from((atts >> 2) & 1);
                        reg.otyper.write(|w| unsafe {
                            w.bits(
                                (reg.otyper.read().bits() & !mask_1)
                                    | (otyper_val * mask_1),
                            )
                        });
                        // OSPEEDR contains 16x 2-bit fields.
                        let ospeedr_val = u32::from((atts >> 3) & 0b11);
                        reg.ospeedr.write(|w| unsafe {
                            w.bits(
                                (reg.ospeedr.read().bits() & !mask_2)
                                    | (ospeedr_val * lsbs_2),
                            )
                        });
                        // PUPDR contains 16x 2-bit fields.
                        let pupdr_val = u32::from((atts >> 5) & 0b11);
                        reg.pupdr.write(|w| unsafe {
                            w.bits(
                                (reg.pupdr.read().bits() & !mask_2)
                                    | (pupdr_val * lsbs_2),
                            )
                        });
                        // AFRx contains 8x 4-bit fields.
                        let af_val = u32::from((atts >> 7) & 0b1111);
                        reg.afrl.write(|w| unsafe {
                            w.bits(
                                (reg.afrl.read().bits() & !mask_4l)
                                    | (af_val * lsbs_4l),
                            )
                        });
                        reg.afrh.write(|w| unsafe {
                            w.bits(
                                (reg.afrh.read().bits() & !mask_4h)
                                    | (af_val * lsbs_4h),
                            )
                        });
                        caller.reply(());
                    }
                    Op::SetReset => {
                        let (msg, caller) = msg
                            .fixed::<SetResetRequest, ()>()
                            .ok_or(ResponseCode::BadArg)?;
                        let port = Port::from_u8(msg.port)
                            .ok_or(ResponseCode::BadArg)?;
                        let reg = gpio_reg(port);

                        reg.bsrr
                            .write(|w| unsafe { w.bits(msg.set_reset.get()) });
                        caller.reply(());
                    }
                    Op::ReadInput => {
                        let (msg, caller) = msg
                            .fixed::<u8, u16>()
                            .ok_or(ResponseCode::BadArg)?;
                        let port =
                            Port::from_u8(*msg).ok_or(ResponseCode::BadArg)?;
                        let reg = gpio_reg(port);

                        caller.reply(reg.idr.read().bits() as u16);
                    }
                    Op::Toggle => {
                        let (msg, caller) = msg
                            .fixed::<ToggleRequest, ()>()
                            .ok_or(ResponseCode::BadArg)?;
                        let port = Port::from_u8(msg.port)
                            .ok_or(ResponseCode::BadArg)?;
                        let reg = gpio_reg(port);

                        // Read current pin *output* states.
                        let state = reg.odr.read().bits();
                        // Compute BSRR value to toggle all pins. That is, move
                        // currently set bits into reset position, and set unset
                        // bits.
                        let bsrr_all = state << 16 | state ^ 0xFFFF;
                        // Write that value, but masked as the user requested.
                        let bsrr_mask = u32::from(msg.pins.get()) * 0x1_0001;
                        reg.bsrr
                            .write(|w| unsafe { w.bits(bsrr_all & bsrr_mask) });
                        caller.reply(());
                    }
                }

                Ok(())
            },
        );
    }
}

fn turn_on_all_gpios() {
    let rcc_driver = Rcc::from(RCC.get_task_id());

    for port in 0..11 {
        let pnum = Peripheral::GpioA as u32 + port; // see bits in AHB4ENR
        rcc_driver.enable_clock_raw(pnum).unwrap();
        rcc_driver.leave_reset_raw(pnum).unwrap();
    }
}

fn gpio_reg(port: Port) -> &'static device::gpioa::RegisterBlock {
    match port {
        Port::A => unsafe { &*device::GPIOA::ptr() },
        Port::B => unsafe { &*device::GPIOB::ptr() },
        Port::C => unsafe { &*device::GPIOC::ptr() },
        Port::D => unsafe { &*device::GPIOD::ptr() },
        Port::E => unsafe { &*device::GPIOE::ptr() },
        Port::F => unsafe { &*device::GPIOF::ptr() },
        Port::G => unsafe { &*device::GPIOG::ptr() },
        Port::H => unsafe { &*device::GPIOH::ptr() },
        Port::I => unsafe { &*device::GPIOI::ptr() },
        Port::J => unsafe { &*device::GPIOJ::ptr() },
        Port::K => unsafe { &*device::GPIOK::ptr() },
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
