// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Thermal task.

#![no_std]

use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
pub enum ThermalError {
    InvalidSensor = 1,
    SensorNotPresent = 2,
    SensorTimeout = 3,
    SensorError = 4,
    SensorUnavailable = 5,
    NoReading = 6,
}

impl From<ThermalError> for u16 {
    fn from(rc: ThermalError) -> Self {
        rc as u16
    }
}

impl From<ThermalError> for u32 {
    fn from(rc: ThermalError) -> Self {
        rc as u32
    }
}

impl core::convert::TryFrom<u32> for ThermalError {
    type Error = ();
    fn try_from(rc: u32) -> Result<Self, Self::Error> {
        Self::from_u32(rc).ok_or(())
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
