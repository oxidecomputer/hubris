// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Thermal task.

#![no_std]

use derive_idol_err::IdolError;
use serde::{Deserialize, Serialize};
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum ThermalError {
    InvalidFan = 1,
    InvalidPWM = 2,
    DeviceError = 3,
    NotInManualMode = 4,
    NotInAutoMode = 5,
    AlreadyInAutoMode = 6,
    InvalidWatchdogTime = 7,
    InvalidParameter = 8,
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, Serialize, Deserialize,
)]
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

/// Substates when running in automatic mode
///
/// These are based on `enum ThermalControlState`, but stripped of the
/// associated state data.
#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, Serialize, Deserialize,
)]
pub enum ThermalAutoState {
    Boot,
    Running,
    Overheated,
    Uncontrollable,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
