// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the User LEDs driver.

#![no_std]

use userlib::*;

#[derive(Copy, Clone, Debug)]
pub enum LedError {
    NotPresent = 1,
}

impl From<u32> for LedError {
    fn from(x: u32) -> Self {
        match x {
            1 => LedError::NotPresent,
            _ => panic!(),
        }
    }
}

#[derive(
    Copy, Clone, Debug, Eq, PartialEq, zerocopy::AsBytes, FromPrimitive,
)]
#[repr(u8)]
pub enum LedState {
    Off = 0,
    On = 1,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));

impl UserLeds {
    pub fn led_set(&self, index: usize, state: bool) -> Result<(), LedError> {
        self.led_set_state(
            index,
            match state {
                true => LedState::On,
                false => LedState::Off,
            },
        )
    }
}
