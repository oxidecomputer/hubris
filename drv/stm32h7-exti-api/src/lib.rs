// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the EXTI server

#![no_std]

use bitflags::bitflags;
use derive_idol_err::IdolError;
use drv_stm32xx_gpio_common::Port;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
#[repr(u32)]
pub enum ExtiError {
    AlreadyRegistered = 1,
    InvalidIndex,
    NotOwner,
    NotRegistered,
}

bitflags! {

    pub struct Edge: u8 {
        const RISING  = 0b01;
        const FALLING = 0b10;
        const RISING_AND_FALLING = Self::RISING.bits | Self::FALLING.bits;
    }

}

impl Exti {

    pub fn enable_gpio(&self, port: Port, index: usize, edges: Edge, notification: u32) -> Result<(), ExtiError> {
        self.enable_gpio_raw(port, index, edges.bits, notification)
    }

}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
