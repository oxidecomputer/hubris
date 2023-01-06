// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for QSFP transceiver managment

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
use serde::{Deserialize, Serialize};
use task_sensor_api::{config::other_sensors, SensorId};
use userlib::{sys_send, FromPrimitive};
use zerocopy::{AsBytes, FromBytes};

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum TransceiversError {
    FpgaError = 1,
    InvalidPortNumber,
    InvalidNumberOfBytes,
    InvalidPowerState,
    #[idol(server_death)]
    ServerRestarted,
}

impl From<FpgaError> for TransceiversError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

/// Each field is a bitmask of the 32 transceivers in big endian order, which
/// results in Port 31 being bit 31, and so forth.
#[derive(Copy, Clone, FromBytes, AsBytes)]
#[repr(C)]
pub struct ModulesStatus {
    pub enable: u32,
    pub reset: u32,
    pub lpmode_txdis: u32,
    pub power_good: u32,
    pub present: u32,
    pub irq_rxlos: u32,
    pub power_good_timeout: u32,
    pub power_good_fault: u32,
}

/// The power states we use to model transceiver state
/// A4 - Module not present.
/// A3 - Module present and powered off.
/// A2 - Module powered, out of reset, and in low-power mode
/// A0 - Module is in high-power mode.
/// Fault - A power fault has ocurred and must be cleared.
#[derive(
    Copy, Clone, PartialEq, Eq, FromPrimitive, AsBytes, Serialize, Deserialize,
)]
#[repr(u8)]
pub enum PowerState {
    A4 = 0,
    A3 = 1,
    A2 = 2,
    A0 = 3,
    Fault = 4,
}

impl TryFrom<u8> for PowerState {
    type Error = ();
    fn try_from(v: u8) -> Result<PowerState, Self::Error> {
        match v {
            0 => Ok(PowerState::A4),
            1 => Ok(PowerState::A3),
            2 => Ok(PowerState::A2),
            3 => Ok(PowerState::A0),
            4 => Ok(PowerState::Fault),
            _ => Err(()),
        }
    }
}

/// Struct to wrap array of PowerState because humility currently does not
/// handle arrays.
#[derive(Copy, Clone, PartialEq, Eq, AsBytes, Serialize, Deserialize)]
#[repr(C)]
pub struct PowerStatesAll([PowerState; 32]);

impl PowerStatesAll {
    pub fn new(states: [PowerState; 32]) -> Self {
        Self(states)
    }
}

/// Size in bytes of a page section we will read or write
///
/// QSFP module's internal memory map is 256 bytes, with the lower 128 being
/// static and then the upper 128 are paged in. The internal address register
/// is only 7 bits, so you can only access half in any single transaction and
/// thus our communication mechanisms have been designed for that.
/// See SFF-8636 and CMIS specifications for details.
pub const PAGE_SIZE_BYTES: usize = 128;

/// The only instantiation of Front IO board that exists is one with 32 QSFP
/// ports.
pub const NUM_PORTS: u8 = 32;

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
////////////////////////////////////////////////////////////////////////////////

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
