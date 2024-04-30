// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{fits_in_ram, HandoffData, UPDATE_RANGE};
use core::ops::Range;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};

unsafe impl HandoffData for RotBootStateV2 {
    const VERSION: u32 = 0;
    const MAGIC: [u8; 12] = *b"whatwhatwhat";
    const MEM_RANGE: Range<usize> = UPDATE_RANGE;
}

/// The pre-kernel Hubris code evaluates all flash slots.
/// It is expected that the stage0 and running Hubris image will be ok.
/// The information on stage0next and the other Hubris partition is used
/// by the update_server to qualify stage0next before promotion to stage0 and
/// can be used by the control plane to diagnose failing updates.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum ImageError {
    /// Image has not been sanity checked (internal use)
    Unchecked = 1,
    /// First page of image is erased.
    FirstPageErased,
    /// Some pages in the image are erased.
    PartiallyProgrammed,
    /// The NXP image offset + length caused a wrapping add.
    InvalidLength,
    /// The header flash page is erased.
    HeaderNotProgrammed,
    /// An image not requiring an ImageHeader is too short.
    BootloaderTooSmall,
    /// A required ImageHeader is missing.
    BadMagic,
    /// The image size in ImageHeader is unreasonable.
    HeaderImageSize,
    /// total_image_length in ImageHeader is not properly aligned.
    UnalignedLength,
    /// Some NXP image types are not supported.
    UnsupportedType,
    /// Wrong format reset vector.
    ResetVectorNotThumb2,
    /// Reset vector points outside of image execution range.
    ResetVector,
    /// Signature check on image failed.
    Signature,
}

/// Top-level type describing images loaded into flash on the RoT.
///
/// This data is injected into RAM at `UPDATE_RANGE` by stage0.
///
/// It gets read from RAM by the `lpc55-update-server`
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct RotBootState {
    pub active: RotSlot,
    pub a: Option<RotImageDetails>,
    pub b: Option<RotImageDetails>,
}

impl RotBootState {
    pub fn active_image(&self) -> Option<RotImageDetails> {
        match self.active {
            RotSlot::A => self.a,
            RotSlot::B => self.b,
        }
    }
}

impl From<RotBootStateV2> for RotBootState {
    // Conversion to handle deprecated APIs.
    fn from(v2: RotBootStateV2) -> Self {
        let a = match v2.a.status {
            Ok(_status) => Some(RotImageDetails {
                digest: v2.a.digest,
                version: ImageVersion {
                    version: 0,
                    epoch: 0,
                },
            }),
            Err(_) => None,
        };
        let b = match v2.b.status {
            Ok(_status) => Some(RotImageDetails {
                digest: v2.b.digest,
                version: ImageVersion {
                    version: 0,
                    epoch: 0,
                },
            }),
            Err(_) => None,
        };
        RotBootState {
            active: v2.active,
            a,
            b,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct RotBootStateV2 {
    pub active: RotSlot,
    pub a: RotImageDetailsV2,
    pub b: RotImageDetailsV2,
    pub stage0: RotImageDetailsV2,
    pub stage0next: RotImageDetailsV2,
}

impl RotBootStateV2 {
    pub fn active_image(&self) -> Option<RotImageDetails> {
        RotBootState::from(*self).active_image()
    }
}

fits_in_ram!(RotBootStateV2);

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct RotImageDetails {
    /// The SHA3-256 measurement of all programmed pages in the flash slot.
    pub digest: [u8; 32],
    /// ImageVersion is not used anywhere and should be deprecated.
    pub version: ImageVersion,
}

/// A measurement of all programmed pages in the flash slot.
///
/// FWID is not a simple digest of the image bytes.
/// The image is padded to the next native flash page with 0xff bytes.
/// Any additional programmed pages in the flash slot beyond the image are
/// included.
/// If there is no valid image, then the digest is over all the programmed
/// pages.
///
/// The intent is to detect incomplete updates where unused pages are not
/// erased or possible exfiltration of date in the unused pages.
///
/// The unused pages in a flash slot could be put to use in some later release
/// to store state. But, no use case has been identified at this time.
///
/// Note that no page is supposed to be partially programmed/partially erased.
/// The LPC55 might reasonably report any page that does not have the final
/// internal ECC syndrome written as being erased. If that
/// is the case, then one would not expect to
/// TODO: There should be testing that creates a partially programmed page if
/// that is possible.
///
#[derive(
    Copy, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum Fwid {
    /// Image should have been written with the last flash page padded with
    /// 0xff bytes. All non-erased pages in the flash slot are included in the
    /// digest.
    Sha3_256([u8; 32]),
}

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct RotImageDetailsV2 {
    pub digest: [u8; 32],
    // Image is valid and properly signed or has a specific ImageError to help find the problem.
    pub status: Result<(), ImageError>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct ImageVersion {
    pub epoch: u32,
    pub version: u32,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum RotSlot {
    A = 0,
    B = 1,
}
