// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the User LEDs driver.

#![no_std]

use core::cell::Cell;
use zerocopy::AsBytes;

use userlib::*;

#[derive(FromPrimitive)]
enum Op {
    On = 1,
    Off = 2,
    Toggle = 3,
}

#[derive(Clone, Debug)]
pub struct UserLeds(Cell<TaskId>);

impl From<TaskId> for UserLeds {
    fn from(t: TaskId) -> Self {
        Self(Cell::new(t))
    }
}

#[derive(Copy, Clone, Debug)]
pub enum LedError {
    Unsupported = 1,
    NoSuchLed = 2,
}

impl From<u32> for LedError {
    fn from(x: u32) -> Self {
        match x {
            1 => LedError::Unsupported,
            2 => LedError::NoSuchLed,
            _ => panic!(),
        }
    }
}

impl UserLeds {
    /// Turns an LED on by index.
    pub fn led_on(&self, index: usize) -> Result<(), LedError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct On(usize);

        impl hl::Call for On {
            const OP: u16 = Op::On as u16;
            type Response = ();
            type Err = LedError;
        }

        hl::send_with_retry(&self.0, &On(index))
    }

    /// Turns an LED off by index.
    pub fn led_off(&self, index: usize) -> Result<(), LedError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct Off(usize);

        impl hl::Call for Off {
            const OP: u16 = Op::Off as u16;
            type Response = ();
            type Err = LedError;
        }

        hl::send_with_retry(&self.0, &Off(index))
    }

    /// Toggles an LED by index.
    pub fn led_toggle(&self, index: usize) -> Result<(), LedError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct Tog(usize);

        impl hl::Call for Tog {
            const OP: u16 = Op::Toggle as u16;
            type Response = ();
            type Err = LedError;
        }

        hl::send_with_retry(&self.0, &Tog(index))
    }
}
