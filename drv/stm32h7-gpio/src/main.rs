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
use zerocopy::{AsBytes, FromBytes, Unaligned, U16};

use drv_stm32xx_gpio_common::{server::get_gpio_regs, Port};

#[derive(FromPrimitive)]
enum Op {
    Configure = 1,
    SetReset = 2,
    ReadInput = 3,
    Toggle = 4,
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
    set: U16<LittleEndian>,
    reset: U16<LittleEndian>,
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
                        let reg = unsafe { get_gpio_regs(port) };
                        reg.configure(
                            msg.pins.get(),
                            msg.packed_attributes.get(),
                        );
                        caller.reply(());
                    }
                    Op::SetReset => {
                        let (msg, caller) = msg
                            .fixed::<SetResetRequest, ()>()
                            .ok_or(ResponseCode::BadArg)?;
                        let port = Port::from_u8(msg.port)
                            .ok_or(ResponseCode::BadArg)?;
                        let reg = unsafe { get_gpio_regs(port) };
                        reg.set_reset(msg.set.get(), msg.reset.get());
                        caller.reply(());
                    }
                    Op::ReadInput => {
                        let (msg, caller) = msg
                            .fixed::<u8, u16>()
                            .ok_or(ResponseCode::BadArg)?;
                        let port =
                            Port::from_u8(*msg).ok_or(ResponseCode::BadArg)?;
                        let reg = unsafe { get_gpio_regs(port) };

                        caller.reply(reg.read());
                    }
                    Op::Toggle => {
                        let (msg, caller) = msg
                            .fixed::<ToggleRequest, ()>()
                            .ok_or(ResponseCode::BadArg)?;
                        let port = Port::from_u8(msg.port)
                            .ok_or(ResponseCode::BadArg)?;
                        let reg = unsafe { get_gpio_regs(port) };
                        reg.toggle(msg.pins.get());
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
