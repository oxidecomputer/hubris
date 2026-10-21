// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use userlib::{FromPrimitive, sys_send};
use zerocopy::{Immutable, IntoBytes, KnownLayout};

#[derive(
    Copy, Clone, Debug, FromPrimitive, Immutable, IntoBytes, KnownLayout,
)]
#[repr(u8)]
pub enum Gpio {
    Gpio0 = 0,
    Gpio1 = 1,
    Gpio2 = 2,
    Gpio3 = 3,
    Gpio4 = 4,
    Gpio5 = 5,
    Gpio6 = 6,
    Gpio7 = 7,
    Gpio8 = 8,
    Gpio9 = 9,
    Gpio10 = 10,
    Gpio11 = 11,
    Gpio12 = 12,
    Gpio13 = 13,
    Gpio14 = 14,
    Gpio15 = 15,
    Gpio16 = 16,
    Gpio17 = 17,
    Gpio18 = 18,
    Gpio19 = 19,
    Gpio20 = 20,
    Gpio21 = 21,
    Gpio22 = 22,
    Gpio23 = 23,
    Gpio24 = 24,
    Gpio25 = 25,
    Gpio26 = 26,
    Gpio27 = 27,
    Gpio28 = 28,
    Gpio29 = 29,
}

impl Gpio {
    pub const fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Immutable, IntoBytes, KnownLayout,
)]
#[repr(u8)]
pub enum Direction {
    In,
    Out,
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Immutable, IntoBytes, KnownLayout,
)]
#[repr(u8)]
pub enum Value {
    High = 1,
    Low = 0,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
