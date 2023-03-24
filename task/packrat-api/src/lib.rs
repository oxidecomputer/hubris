// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the VPD task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;
use zerocopy::{AsBytes, FromBytes, LittleEndian, U16};

pub use oxide_barcode::VpdIdentity;

/// Represents a range of allocated MAC addresses, per RFD 320
///
/// The SP will claim the first `N` addresses based on VLAN configuration
/// (typically either 1 or 2).
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromBytes, AsBytes, Default)]
#[repr(C)]
pub struct MacAddressBlock {
    pub base_mac: [u8; 6],
    pub count: U16<LittleEndian>,
    pub stride: u8,
}

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum CacheGetError {
    ValueNotSet = 1,
}

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum CacheSetError {
    ValueAlreadySet = 1,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
