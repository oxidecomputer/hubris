// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Control Plane Agent task.

#![no_std]

use derive_idol_err::IdolError;
pub use host_sp_messages::HostStartupOptions;
use serde::{Deserialize, Serialize};
use static_assertions::const_assert;
use userlib::*;
use zerocopy::{AsBytes, FromBytes};

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum ControlPlaneAgentError {
    DataUnavailable = 1,
    InvalidStartupOptions,
    OperationUnsupported,
    MgsAttachedToUart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UartClient {
    Mgs,
    Humility,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, FromBytes, AsBytes)]
#[repr(C, packed)]
pub struct Identity {
    pub part_number: [u8; Self::PART_NUMBER_LEN],
    pub revision: u32,
    pub serial: [u8; Self::SERIAL_LEN],
}

impl Identity {
    pub const PART_NUMBER_LEN: usize = 11;
    pub const SERIAL_LEN: usize = 11;
}

impl From<Identity> for host_sp_messages::Identity {
    fn from(id: Identity) -> Self {
        // The Host/SP protocol has larger fields for model/serial than we
        // use currently; statically assert that we haven't outgrown them.
        const_assert!(
            Identity::PART_NUMBER_LEN <= host_sp_messages::Identity::MODEL_LEN
        );
        const_assert!(
            Identity::SERIAL_LEN <= host_sp_messages::Identity::SERIAL_LEN
        );

        let mut new_id = Self::default();
        new_id.model[..id.part_number.len()].copy_from_slice(&id.part_number);
        new_id.revision = id.revision;
        new_id.serial[..id.serial.len()].copy_from_slice(&id.serial);
        new_id
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
