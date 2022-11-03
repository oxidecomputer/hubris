// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Ignition server.

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
use drv_sidecar_mainboard_controller::ignition::*;
use idol_runtime::ServerDeath;
use userlib::{sys_send, FromPrimitive, ToPrimitive};

#[derive(
    Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, IdolError,
)]
pub enum IgnitionError {
    ServerDied,
    FpgaError,
    InvalidValue,
    Nack,
    Timeout,
}

impl From<ServerDeath> for IgnitionError {
    fn from(_e: ServerDeath) -> Self {
        Self::ServerDied
    }
}

impl From<FpgaError> for IgnitionError {
    fn from(_e: FpgaError) -> Self {
        Self::FpgaError
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
