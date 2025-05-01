// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Thermal task.

#![no_std]

use derive_idol_err::IdolError;
use drv_i2c_api::ResponseCode;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::{units::Celsius, *};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
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
    FanControllerUninitialized = 10,

    #[idol(server_death)]
    ServerDeath,
}

#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    SerializedSize,
    counters::Count,
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
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    SerializedSize,
    counters::Count,
)]
pub enum ThermalAutoState {
    Boot,
    Running,
    Overheated,
    Uncontrollable,
}

/// Properties for a particular part in the system
#[derive(Clone, Copy, IntoBytes, FromBytes, Immutable, KnownLayout)]
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

/// All of these functions take an **instantaneous** temperature; to convert a
/// timestamped reading into an instantaneous temperature (using a thermal
/// model), see `TimestampedTemperatureReading::worst_case`.
impl ThermalProperties {
    /// Returns whether this part is exceeding its power-down temperature
    pub fn should_power_down(&self, t: Celsius) -> bool {
        t.0 >= self.power_down_temperature.0
    }

    /// Returns whether this part is exceeding its critical temperature
    pub fn is_critical(&self, t: Celsius) -> bool {
        t.0 >= self.critical_temperature.0
    }

    /// Returns whether this part is below its critical temperature, with
    /// a user-configured hysteresis band.
    pub fn is_sub_critical(&self, t: Celsius, hysteresis: Celsius) -> bool {
        t.0 < self.critical_temperature.0 - hysteresis.0
    }

    /// Returns the margin of this part, given a current temperature reading.
    ///
    /// Positive margin means that the part is below its max temperature;
    /// negative means that it's overheating.
    pub fn margin(&self, t: Celsius) -> Celsius {
        Celsius(self.target_temperature.0 - t.0)
    }
}

/// Combined error type for all of our temperature sensors
///
/// Most of them will only return an I2C `ResponseCode`, but in some cases,
/// they can report an error through in-band signalling (looking at you, NVMe)
#[derive(
    Copy,
    Clone,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    SerializedSize,
    counters::Count,
)]
pub enum SensorReadError {
    I2cError(#[count(children)] ResponseCode),

    /// The sensor reported that data is either not present or too old
    NoData,

    /// The sensor reported a failure
    SensorFailure,

    /// The returned value is listed as reserved in the datasheet and does not
    /// represent a temperature.
    ReservedValue,

    /// The reply is structurally incorrect (wrong length, bad checksum, etc)
    CorruptReply,
}

impl From<drv_i2c_devices::tmp117::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::tmp117::Error) -> Self {
        use drv_i2c_devices::tmp117::Error::*;
        match s {
            BadRegisterRead { code, .. } => Self::I2cError(code),
        }
    }
}

impl From<drv_i2c_devices::tmp451::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::tmp451::Error) -> Self {
        use drv_i2c_devices::tmp451::Error::*;
        match s {
            BadRegisterRead { code, .. } => Self::I2cError(code),
            BadRegisterWrite { .. } => panic!(),
        }
    }
}

impl From<drv_i2c_devices::sbtsi::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::sbtsi::Error) -> Self {
        use drv_i2c_devices::sbtsi::Error::*;
        match s {
            BadRegisterRead { code, .. } => Self::I2cError(code),
        }
    }
}

impl From<drv_i2c_devices::tse2004av::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::tse2004av::Error) -> Self {
        use drv_i2c_devices::tse2004av::Error::*;
        match s {
            BadRegisterRead { code, .. } => Self::I2cError(code),
        }
    }
}

impl From<drv_i2c_devices::nvme_bmc::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::nvme_bmc::Error) -> Self {
        use drv_i2c_devices::nvme_bmc::Error::*;
        match s {
            I2cError(v) => Self::I2cError(v),
            NoData => Self::NoData,
            SensorFailure => Self::SensorFailure,
            Reserved => Self::ReservedValue,
            InvalidLength | BadChecksum => Self::CorruptReply,
        }
    }
}

impl From<SensorReadError> for task_sensor_api::NoData {
    fn from(code: SensorReadError) -> task_sensor_api::NoData {
        match code {
            SensorReadError::I2cError(v) => v.into(),
            _ => Self::DeviceError,
        }
    }
}

impl From<drv_i2c_devices::pct2075::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::pct2075::Error) -> Self {
        use drv_i2c_devices::pct2075::Error::*;
        match s {
            BadTempRead { code, .. } => Self::I2cError(code),
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
