// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the User LEDs driver.

#![no_std]

use userlib::*;

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum IdolTestError {
    UhOh = 1,
}
impl TryFrom<u32> for IdolTestError {
    type Error = ();
    fn try_from(x: u32) -> Result<Self, Self::Error> {
        Self::from_u32(x).ok_or(())
    }
}
impl From<IdolTestError> for u16 {
    fn from(rc: IdolTestError) -> Self {
        rc as u16
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
