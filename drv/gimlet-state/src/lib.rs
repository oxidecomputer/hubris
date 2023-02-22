// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common definitions for dealing with system state on Gimlet, shared by the
//! various tasks and IPC interfaces involved.

#![no_std]

use userlib::FromPrimitive;
use zerocopy::AsBytes;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, Eq, AsBytes)]
#[repr(u8)]
pub enum PowerState {
    A2 = 1,
    A2PlusMono = 2,
    A2PlusFans = 3,
    A1 = 4,
    A0 = 5,
    A0PlusHP = 6,
    A0Thermtrip = 7,
    A0Reset = 8,
}
