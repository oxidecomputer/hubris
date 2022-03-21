// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Thermal task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, IdolError)]
pub enum ThermalError {
    InvalidFan = 1,
    InvalidPWM = 2,
    DeviceError = 3,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
