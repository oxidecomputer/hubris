// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Medusa Sequencer server.

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::{sys_send, FromPrimitive};

#[derive(
    Copy, Clone, Debug, PartialEq, Deserialize, Serialize, SerializedSize,
)]
pub enum RailName {
    V1P0Mgmt,
    V1P2Mgmt,
    V2P5Mgmt,
    V1P0FrontPhy,
    V2P5FrontPhy,
    V1P0LocalPhy,
    V2P5LocalPhy,
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum MedusaError {
    FpgaError = 1,
    NoFrontIOBoard,
    // The Front IO board power faulted
    FrontIOBoardPowerFault,
    // An power supply on Medusa faulted
    PowerFault,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<FpgaError> for MedusaError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
