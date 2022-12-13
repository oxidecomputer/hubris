// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{fits_in_ram, HandoffData, UPDATE_RANGE};
use core::ops::Range;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};

unsafe impl HandoffData for RotUpdateDetails {
    const VERSION: u32 = 0;
    const MAGIC: [u8; 12] = *b"whatwhatwhat";
    const MEM_RANGE: Range<usize> = UPDATE_RANGE;
}

/// Top-level type describing images loaded into flash on the RoT.
///
/// This data is injected into RAM at `UPDATE_RANGE` by stage0.
///
/// It gets read from RAM by the `lpc55-update-server`
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct RotUpdateDetails {
    pub active: RotSlot,
    pub a: Option<RotImageDetails>,
    pub b: Option<RotImageDetails>,
}

fits_in_ram!(RotUpdateDetails);

#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct RotImageDetails {
    pub digest: [u8; 32],
    pub version: ImageVersion,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct ImageVersion {
    pub epoch: u32,
    pub version: u32,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum RotSlot {
    Stage0 = 0,
    A = 1,
    B = 2,
}
