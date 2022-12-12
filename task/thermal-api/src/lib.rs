// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Thermal task.

#![no_std]

use derive_idol_err::IdolError;
use serde::{Deserialize, Serialize};
use userlib::{units::Celsius, *};
use zerocopy::{AsBytes, FromBytes};

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
    InvalidIndex = 9,

    #[idol(server_death)]
    ServerDeath,
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

/// Properties for a particular part in the system
#[derive(Clone, Copy, AsBytes, FromBytes)]
#[repr(C)]
pub struct ThermalProperties {
    /// Target temperature for this part
    pub target_temperature: Celsius,

    /// At the critical temperature, we should turn the fans up to 100% power in
    /// an attempt to cool the part.
    pub critical_temperature: Celsius,

    /// Temperature at which we drop into the A2 power state.  This should be
    /// below the part's nonrecoverable temperature.
    pub power_down_temperature: Celsius,

    /// Maximum slew rate of temperature, measured in °C per second
    ///
    /// The slew rate is used to model worst-case temperature if we haven't
    /// heard from a chip in a while (e.g. due to dropped samples)
    pub temperature_slew_deg_per_sec: f32,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
