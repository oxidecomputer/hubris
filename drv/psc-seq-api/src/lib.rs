// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Sequencer server.

#![no_std]

use userlib::FromPrimitive;

#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u8)]
pub enum PowerState {
    Init = 0,
    A2 = 1,
}
