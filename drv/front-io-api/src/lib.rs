// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Front IO server.

#![no_std]

use crate::phy_smi::PhyOscState;
use counters::Count;
use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
use drv_fpga_user_api::power_rail::PowerRailStatus;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;

// Re-export for use in the server
pub use transceiver_messages::message::LedState;

pub mod controller;
pub mod leds;
pub mod phy_smi;
pub mod transceivers;

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, Count,
)]
pub enum FrontIOError {
    FpgaError = 1,
    NotPresent,
    NotReady,
    InvalidPortNumber,
    InvalidNumberOfBytes,
    InvalidPhysicalToLogicalMap,
    InvalidModuleResult,
    LedInitFailure,
    PowerNotGood,
    PowerFault,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<FpgaError> for FrontIOError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    Count,
    Eq,
    PartialEq,
    Deserialize,
    Serialize,
    SerializedSize,
)]
pub enum FrontIOStatus {
    /// Start state
    Init,
    /// No board detected
    NotPresent,
    /// The FPGAs are being configured
    FpgaInit,
    /// Confirming that the PHY oscillator is behaving
    OscInit,
    /// Board is present and fully operational
    Ready,
}

include!(concat!(
    env!("OUT_DIR"),
    "/sidecar_qsfp_x32_controller_regs.rs"
));

use crate::transceivers::{
    LogicalPort, LogicalPortMask, ModuleResult, ModuleResultNoFailure,
    PortI2CStatus, TransceiverStatus,
};
include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
