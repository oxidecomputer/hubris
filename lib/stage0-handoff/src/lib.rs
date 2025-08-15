// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![cfg_attr(not(test), no_std)]

use core::ops::Range;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use static_assertions::const_assert;

mod rot_update_details;

pub use rot_update_details::{
    Fwid, ImageError, ImageVersion, RotBootState, RotBootStateV2,
    RotImageDetails, RotImageDetailsV2, RotSlot,
};

// This memory is the USB peripheral SRAM that's 0x4000 bytes long. Changes
// to this address must be coordinated with the [dice_*] tables in
// chips/lpc55/chip.toml
// TODO: get from app.toml -> chip.toml at build time
pub const MEM_RANGE: Range<usize> = 0x4010_0000..0x4010_4000;
pub const DICE_RANGE: Range<usize> = 0x4010_0000..0x4010_2000;
pub const UPDATE_RANGE: Range<usize> = 0x4010_2000..0x4010_3000;

const_assert!(MEM_RANGE.start <= DICE_RANGE.start);
const_assert!(DICE_RANGE.end <= UPDATE_RANGE.start);
const_assert!(UPDATE_RANGE.end <= MEM_RANGE.end);
/// The error returned when `HandoffData::load` fails.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
)]
pub enum HandoffDataLoadError {
    Deserialize,
    BadMagic,
    UnexpectedVersion(u32),
}

impl From<hubpack::Error> for HandoffDataLoadError {
    fn from(_: hubpack::Error) -> Self {
        HandoffDataLoadError::Deserialize
    }
}

/// The header that prefixes each serialized `HandoffData` structure.
///
/// We put the version first so the data type can be updated, and we follow
/// by a magic number for visibility and debugging.
///
/// Hubpack serializes integers as little endian and also versions arrays by
/// writing them directly, so this header serializes directly.
///
/// The 16 byte size of the header also fits nicely on one line in hexdumps.
#[derive(Deserialize, Serialize, SerializedSize)]
pub struct HandoffDataHeader {
    pub version: u32,
    pub magic: [u8; 12],
}

// Types that can be transfered through the memory region used to pass DICE
// artifacts from stage0 to hubris tasks.
//
// This trait cannot check the validity of the memory range selected by
// implementers and so implementers of this trait are required to ensure that
// the range denoted by Self::MEM_RANGE is:
// - within the memory range used to hold DICE artifacts
// - large enough to contain a the largest serialized form of the implementing
// type
// - non-overlapping with the ranges of memory used by other implementers of
// this trait
pub unsafe trait HandoffData {
    const VERSION: u32;
    const MAGIC: [u8; 12];
    const MEM_RANGE: Range<usize>;

    fn header() -> HandoffDataHeader {
        HandoffDataHeader {
            version: Self::VERSION,
            magic: Self::MAGIC,
        }
    }

    /// Load the serialized data put in memory by stage0
    fn load() -> Result<Self, HandoffDataLoadError>
    where
        Self: SerializedSize + Sized,
        for<'d> Self: Deserialize<'d>,
    {
        // Cast the MEM_START address to a slice of bytes of MAX_SIZE length.
        //
        // Safety: This unsafe block relies on implementers of the trait to
        // validate the memory range denoted by Self::MEM_RANGE. Each
        // implementation in this module is checked by static assertion.
        let src = unsafe {
            core::slice::from_raw_parts_mut(
                Self::MEM_RANGE.start as *mut u8,
                Self::MAX_SIZE + HandoffDataHeader::MAX_SIZE,
            )
        };

        let (header, rest) = hubpack::deserialize::<HandoffDataHeader>(src)?;
        if header.version != Self::VERSION {
            return Err(HandoffDataLoadError::UnexpectedVersion(
                header.version,
            ));
        }
        if header.magic != Self::MAGIC {
            return Err(HandoffDataLoadError::BadMagic);
        }

        let (data, _) = hubpack::deserialize::<Self>(rest)?;
        Ok(data)
    }

    fn load_from_addr(src: &[u8]) -> Result<Self, HandoffDataLoadError>
    where
        Self: SerializedSize + Sized,
        for<'d> Self: Deserialize<'d>,
    {
        let (header, rest) = hubpack::deserialize::<HandoffDataHeader>(src)?;
        if header.version != Self::VERSION {
            return Err(HandoffDataLoadError::UnexpectedVersion(
                header.version,
            ));
        }
        if header.magic != Self::MAGIC {
            return Err(HandoffDataLoadError::BadMagic);
        }

        let (data, _) = hubpack::deserialize::<Self>(rest)?;
        Ok(data)
    }
}

/// Assert at compile time that the given serialized `HandoffData`
/// implementation fits in the allocated `MEM_RANGE`
#[macro_export]
macro_rules! fits_in_ram {
    ($data:tt) => {
        static_assertions::const_assert!(
            $data::MEM_RANGE.end - $data::MEM_RANGE.start
                >= $data::MAX_SIZE + $crate::HandoffDataHeader::MAX_SIZE
        );
    };
}
