// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the VPD task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

pub use oxide_barcode::VpdIdentity;
pub use task_net_api::MacAddressBlock;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum CacheGetError {
    ValueNotSet = 1,
}

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum CacheSetError {
    ValueAlreadySet = 1,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
