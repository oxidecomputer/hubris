// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the 'attest' task.

#![no_std]

use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::sys_send;

#[derive(
    Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize, SerializedSize,
)]
pub enum AttestError {
    CertTooBig,
    InvalidCertIndex,
    NoCerts,
    OutOfRange,
    MeasurementLogFull,
    TaskRestarted,
    BadLease,
    UnsupportedAlgorithm,
}

impl From<idol_runtime::ServerDeath> for AttestError {
    fn from(_: idol_runtime::ServerDeath) -> Self {
        AttestError::TaskRestarted
    }
}

#[derive(
    Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize, SerializedSize,
)]
pub enum HashAlgorithm {
    Sha3_256,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
