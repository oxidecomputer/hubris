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
            _ => unreachable!(), // Unreachable by inspection.
        }
    }
}

impl From<RotBootStateV2> for RotBootState {
    // Conversion to handle deprecated APIs.
    fn from(v2: RotBootStateV2) -> Self {
        let a = match v2.a.version {
            Ok(v) => Some(RotImageDetails {
                digest: v2.a.digest,
                version: v,
            }),
            Err(_) => None,
        };
        let b = match v2.b.version {
            Ok(v) => Some(RotImageDetails {
                digest: v2.b.digest,
                version: v,
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
    // The SHA3-256 measurement of all programmed pages in the flash slot.
    pub digest: [u8; 32],
    pub version: ImageVersion,
}

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct RotImageDetailsV2 {
    // The SHA3-256 measurement of all programmed pages in the flash slot.
    pub digest: [u8; 32],
    pub version: Result<ImageVersion, ImageError>,
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
    Stage0 = 2,
    Stage0Next = 3,
}
