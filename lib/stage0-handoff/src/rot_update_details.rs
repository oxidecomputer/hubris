// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{fits_in_ram, HandoffData, UPDATE_RANGE};
use core::ops::Range;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};

unsafe impl HandoffData for RotBootState {
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
    Short,
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
    /// A CRC validated image has an invalid CRC.
    Crc,
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
    pub a: RotImageDetails,
    pub b: RotImageDetails,
    pub stage0: RotImageDetails,
    pub stage0next: RotImageDetails,
}

impl RotBootState {
    pub fn active_image(&self) -> RotImageDetails {
        match self.active {
            RotSlot::A => self.a.clone(),
            RotSlot::B => self.b.clone(),
        }
    }
}

fits_in_ram!(RotBootState);

#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct RotImageDetails {
    // The SHA3-256 measurement of all programmed pages in the flash slot.
    pub digest: [u8; 32],
    pub version: ImageVersion,
    // Image sanity check and signature validation.
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
