// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Thermal task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum ThermalError {
    InvalidFan = 1,
    InvalidPWM = 2,
    DeviceError = 3,
    NotInManualMode = 4,
    NoReading = 5,
    InvalidWatchdogTime = 6,
}

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq)]
pub enum ThermalMode {
    /// The thermal loop has not started.  This is the initial state, but
    /// should be transient, as the thermal task turns on.
    Off = 0,
    /// The thermal loop is polling sensors and sending data to the `sensors`
    /// task, but not setting fan PWM speeds.
    Manual = 1,
    /// The thermal loop is polling sensors and sending data to the `sensors`
    /// task; fan speeds are controlled based on certain temperature sensors.
    Auto = 2,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
