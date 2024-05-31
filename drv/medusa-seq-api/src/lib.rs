// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Medusa Sequencer server.

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
use userlib::{sys_send, FromPrimitive};

#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u8)]
pub enum PowerState {
    Init = 0,
    A2 = 1,
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum MedusaError {
    FpgaError = 1,
    NoFrontIOBoard,
    FrontIOBoardPowerFault,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<FpgaError> for MedusaError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
