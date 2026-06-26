// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the 'attest' task.

#![no_std]

use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::sys_send;

#[derive(
    Copy,
    Clone,
    Debug,
    Deserialize,
    Eq,
    PartialEq,
    Serialize,
    SerializedSize,
    counters::Count,
)]
pub enum AttestError {
    CertTooBig,
    InvalidCertIndex,
    NoCerts,
    OutOfRange,
    LogFull,
    LogTooBig,
    TaskRestarted,
    BadLease,
    UnsupportedAlgorithm,
    SerializeLog,
    SerializeSignature,
    SignatureTooBig,
    ReservedLogSlot,
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

pub const NONCE_MIN_SIZE: usize = 32;
pub const NONCE_MAX_SIZE: usize = 128;

pub const TQ_HASH_SIZE: usize = attest_data::Sha3_256Digest::LENGTH;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
