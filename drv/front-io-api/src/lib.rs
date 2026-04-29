// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use counters::Count;
use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
use userlib::FromPrimitive;

// Re-export for use in the server
pub use transceiver_messages::message::LedState;

#[cfg(feature = "controller")]
pub mod controller;
#[cfg(feature = "leds")]
pub mod leds;
#[cfg(feature = "phy_smi")]
pub mod phy_smi;
#[cfg(feature = "transceivers")]
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
    SeqError,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<FpgaError> for FrontIOError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

include!(concat!(
    env!("OUT_DIR"),
    "/sidecar_qsfp_x32_controller_regs.rs"
));
