// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::{sys_send, FromPrimitive};
use zerocopy::AsBytes;

pub use drv_update_api::*;

/// Minimal error type for caboose actions
///
/// The RoT decodes the caboose location and presence, but does not actually
/// decode any of its contents on-board; as such, this `enum` has fewer variants
/// that the `CabooseError` itself.
///
/// This `enum` is used as part of the wire format for SP-RoT communication, and
/// therefore cannot be changed at will; see discussion in `drv_sprot_api::Msg`
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    IdolError,
    SerializedSize,
    Serialize,
    Deserialize,
)]
pub enum RawCabooseError {
    InvalidRead = 1,
    ReadFailed,
    MissingCaboose,
    NoImageHeader,
}

impl From<RawCabooseError> for drv_caboose::CabooseError {
    fn from(t: RawCabooseError) -> Self {
        match t {
            RawCabooseError::InvalidRead => Self::InvalidRead,
            RawCabooseError::ReadFailed => Self::RawReadFailed,
            RawCabooseError::MissingCaboose => Self::MissingCaboose,
            RawCabooseError::NoImageHeader => Self::NoImageHeader,
        }
    }
}

/// Target for an update operation
///
/// This `enum` is used as part of the wire format for SP-RoT communication, and
/// therefore cannot be changed at will; see discussion in `drv_sprot_api::Msg`
#[repr(u8)]
#[derive(
    FromPrimitive,
    AsBytes,
    Eq,
    PartialEq,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    SerializedSize,
)]
pub enum UpdateTarget {
    // The value of 0 is reserved
    // The value of 1 was previously used for Alternate, when this enum was
    // shared by the RoT and the SP.  Now, it is unused but reserved to avoid
    // changing the wire format.

    // Represents targets where we must write to a specific range
    // of flash.
    ImageA = 2,
    ImageB = 3,
    Bootloader = 4,
}

// This value is currently set to `lpc55_romapi::FLASH_PAGE_SIZE`
//
// We hardcode it for simplicity, and because we cannot,and should not,
// directly include the `lpc55_romapi` crate. While we could transfer
// arbitrary amounts of data over spi and have the update server on
// the RoT split it up, this makes the code more complicated than
// necessary and is only an optimization. For now, especially since we
// only have 1 RoT and we must currently define a constant for use the
// `control_plane_agent::ComponentUpdater` trait, we do the simple thing
// and hardcode according to hardware requirements.
pub const BLOCK_SIZE_BYTES: usize = 512;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
