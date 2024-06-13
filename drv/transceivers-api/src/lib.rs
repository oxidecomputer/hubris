// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for QSFP transceiver managment

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
use drv_front_io_api::transceivers::NUM_PORTS;
use task_sensor_api::{config::other_sensors, SensorId};
use userlib::FromPrimitive;

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum TransceiversError {
    FpgaError = 1,
    InvalidPowerState,
    LedI2cError,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<FpgaError> for TransceiversError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

////////////////////////////////////////////////////////////////////////////////

pub const TRANSCEIVER_TEMPERATURE_SENSORS: [SensorId; NUM_PORTS as usize] = [
    other_sensors::QSFP_XCVR0_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR1_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR2_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR3_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR4_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR5_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR6_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR7_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR8_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR9_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR10_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR11_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR12_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR13_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR14_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR15_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR16_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR17_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR18_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR19_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR20_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR21_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR22_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR23_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR24_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR25_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR26_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR27_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR28_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR29_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR30_TEMPERATURE_SENSOR,
    other_sensors::QSFP_XCVR31_TEMPERATURE_SENSOR,
];
