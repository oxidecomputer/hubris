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

/// Firmware ID - A measurement of all programmed pages in the flash slot.
///
/// The digest is over the image bytes including the last partial flash
/// page which is padded with 0xff bytes when writing. All other flash
/// pages are expected to be erased (not readable on the LPC55).
/// However, any additional programmed pages found in the flash slot
/// are also included in the FWID.
/// The intent of including the additional pages is to detect incomplete
/// update operations where unused pages are not erased. The possible
/// attempted exfiltration of date in the unused pages can also be detected
/// in this way.
///
/// If no valid image is detected, then the digest is over all the
/// programmed pages.
///
/// Any completly erased flash slot will return an FWID equal to
/// `const _FWID_ERASED_SLOT` defined below as:
///
///     e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
///
/// Note that no page is supposed to be partially programmed/partially erased.
/// The LPC55 might reasonably report any page that does not have the final
/// internal ECC syndrome written as being erased. If that is the case, then
/// one would not expect to.
///
/// TODO: Test: Try to create a partially programmed page and understand the
/// code behavior in that case.
///
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum Fwid {
    Sha3_256([u8; 32]),
}

const _FWID_ERASED_SLOT: Fwid = Fwid::Sha3_256([
    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8,
    0x99, 0x6f, 0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c,
    0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
]);

/// ROT boot state and preferences retrieved from the lpc55-update-server
///
/// SW version information is in the caboose and is read from a different
/// API
#[derive(Debug, Clone, Serialize, Deserialize, SerializedSize)]
pub struct RotBootInfo {
    /// Ths slot of the currently running image
    pub active: SlotId,
    /// The persistent boot preference written into the current authoritative
    /// CFPA page (ping or pong).
    pub persistent_boot_preference: SlotId,
    /// The persistent boot preference written into the CFPA scratch page that
    /// will become the persistent boot preference in the authoritative CFPA
    /// page upon reboot, unless CFPA update of the authoritative page fails
    /// for some reason.
    pub pending_persistent_boot_preference: Option<SlotId>,
    /// Override persistent preference selection for a single boot
    ///
    /// This is a magic ram value that is cleared by bootleby
    pub transient_boot_preference: Option<SlotId>,
    /// Sha3-256 Digest of Slot A in Flash
    pub slot_a_sha3_256_digest: Option<[u8; 32]>,
    /// Sha3-256 Digest of Slot B in Flash
    pub slot_b_sha3_256_digest: Option<[u8; 32]>,
}

/// ROT boot state and preferences retrieved from the lpc55-update-server
///
/// SW version information is in the caboose and is read from a different
/// API
#[derive(Debug, Clone, Serialize, Deserialize, SerializedSize)]
pub struct RotBootInfoV2 {
    /// Ths slot of the currently running image
    pub active: SlotId,
    /// The persistent boot preference written into the current authoritative
    /// CFPA page (ping or pong).
    pub persistent_boot_preference: SlotId,
    /// The persistent boot preference written into the CFPA scratch page that
    /// will become the persistent boot preference in the authoritative CFPA
    /// page upon reboot, unless CFPA update of the authoritative page fails
    /// for some reason.
    pub pending_persistent_boot_preference: Option<SlotId>,
    /// Override persistent preference selection for a single boot
    ///
    /// This is a magic ram value that is cleared by bootleby
    pub transient_boot_preference: Option<SlotId>,
    /// Digest of Slot A in Flash
    pub slot_a_fwid: Fwid,
    /// Digest of Slot B in Flash
    pub slot_b_fwid: Fwid,
    /// Digest of Stage0 in Flash
    pub stage0_fwid: Fwid,
    /// Digest of Stage0Next in Flash
    pub stage0next_fwid: Fwid,
    /// If readable, the result of checking an image using the ROM code.
    pub slot_a_status: Result<(), ImageError>,
    pub slot_b_status: Result<(), ImageError>,
    pub stage0_status: Result<(), ImageError>,
    pub stage0next_status: Result<(), ImageError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, SerializedSize)]
pub enum VersionedRotBootInfo {
    V1(RotBootInfo),
    V2(RotBootInfoV2),
}

#[derive(Clone, Copy, Serialize, Deserialize, SerializedSize)]
pub enum RotPage {
    // The manufacturing area that cannot be changed
    Cmpa,
    // The field page that is currently active (highest version)
    CfpaActive,
    // The field page that will be applied after the next reboot (assuming
    // version is incremented)
    CfpaScratch,
    // The field page that is not currently active (lower version, ignoring scratch)
    CfpaInactive,
}

/// Target for an update operation
///
/// This `enum` is used as part of the wire format for SP-RoT communication, and
/// therefore cannot be changed at will; see discussion in `drv_sprot_api::Msg`
///
/// In particular, the order of variants cannot change!
#[derive(
    Debug, Eq, PartialEq, Clone, Copy, Serialize, Deserialize, SerializedSize,
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

/// Designate a logical sub-component of the RoT
///
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
pub enum RotComponent {
    Hubris,
    Stage0,
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
    HandoffDataLoadError, ImageError, ImageVersion, RotBootState,
    RotBootStateV2, RotImageDetails, RotSlot,
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
