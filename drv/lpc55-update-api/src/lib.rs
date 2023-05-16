// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::{sys_send, FromPrimitive};

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

/// ROT boot state and preferences retrieved from the lpc55-update-server
///
/// SW version information is in the caboose and is read from a different
/// API
#[derive(Debug, Clone, Serialize, Deserialize, SerializedSize)]
pub struct RotBootInfo {
    pub active: SlotId,

    // The persistent boot preference written into the current authoritative
    // CFPA page (ping or pong).
    pub persistent_boot_preference: SlotId,

    // The persistent boot preference written into the CFPA scratch page that
    // will become the persistent boot preference in the authoritative CFPA
    // page upon reboot, unless CFPA update of the authoritative page fails for
    // some reason.
    pub pending_persistent_boot_preference: Option<SlotId>,

    /// Override persistent preference selection for a single boot
    ///
    /// This is a magic ram value that is cleared by bootleby
    pub transient_boot_preference: Option<SlotId>,
}

/// Target for an update operation
///
/// This `enum` is used as part of the wire format for SP-RoT communication, and
/// therefore cannot be changed at will; see discussion in `drv_sprot_api::Msg`
///
/// In particular, the order of variants cannot change!
#[derive(
    Eq, PartialEq, Clone, Copy, Serialize, Deserialize, SerializedSize,
)]
pub enum UpdateTarget {
    // This variant was previously used for Alternate, when this enum was shared
    // by the RoT and the SP.  Now, it is unused but reserved to avoid changing
    // serialization, which is automatically derived based on variant order.
    _Reserved,

    // Represents targets where we must write to a specific range
    // of flash.
    ImageA,
    ImageB,
    Bootloader,
}

/// Designates a firmware image slot in parts that have fixed slots (rather than
/// bank remapping).
///
/// This `enum` is used as part of the wire format for SP-RoT communication, and
/// therefore cannot be changed at will; see discussion in `drv_sprot_api::Msg`
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    FromPrimitive,
    Serialize,
    Deserialize,
    SerializedSize,
)]
pub enum SlotId {
    A,
    B,
}

impl From<RotSlot> for SlotId {
    fn from(value: RotSlot) -> Self {
        match value {
            RotSlot::A => SlotId::A,
            RotSlot::B => SlotId::B,
        }
    }
}

impl TryFrom<u16> for SlotId {
    type Error = ();
    fn try_from(i: u16) -> Result<Self, Self::Error> {
        Self::from_u16(i).ok_or(())
    }
}

/// When booting into an alternate image, specifies how "sticky" that decision
/// is.
///
/// This `enum` is used as part of the wire format for SP-RoT communication, and
/// therefore cannot be changed at will; see discussion in `drv_sprot_api::Msg`
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    FromPrimitive,
    Serialize,
    Deserialize,
    SerializedSize,
)]
pub enum SwitchDuration {
    /// Choice applies once. Resetting the processor will return to the original
    /// image. Useful when provisionally testing an update, but only available
    /// on certain implementations.
    Once,
    /// Choice is permanent until changed. This is more dangerous, but is also
    /// universally available.
    Forever,
}

// Re-export
pub use stage0_handoff::{
    HandoffDataLoadError, ImageVersion, RotBootState, RotImageDetails, RotSlot,
};

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
