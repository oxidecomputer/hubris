// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for QSFP transceiver managment

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
// use serde::{Deserialize, Serialize};
use userlib::{sys_send, FromPrimitive};

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum TransceiversError {
    FpgaError = 1,
    InvalidPortNumber = 2,
    InvalidNumberOfBytes = 3,
}

impl From<FpgaError> for TransceiversError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

#[derive(Copy, Clone, zerocopy::FromBytes, zerocopy::AsBytes)]
#[repr(C)]
pub struct ModulesStatus {
    pub enable: u32,
    pub reset: u32,
    pub lpmode_txdis: u32,
    pub power_good: u32,
    pub power_good_timeout: u32,
    pub present: u32,
    pub irq_rxlos: u32,
}

impl From<[u32; 7]> for ModulesStatus {
    fn from(data: [u32; 7]) -> Self {
        ModulesStatus {
            enable: data[0],
            reset: data[1],
            lpmode_txdis: data[2],
            power_good: data[3],
            power_good_timeout: data[4],
            present: data[5],
            irq_rxlos: data[6],
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
