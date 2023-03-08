// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the caboose reader task

#![no_std]

use derive_idol_err::IdolError;
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use userlib::{FromPrimitive, UnwrapLite};

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum CabooseError {
    MissingCaboose = 1,
    TlvcReaderBeginFailed,
    TlvcReadExactFailed,
    NoSuchTag,
    BadChecksum,
}

/// Simple handle which points to the beginning of the TLV-C region of the
/// caboose and allows us to implement `TlvcRead`
#[derive(Copy, Clone)]
pub struct CabooseReader {
    base: u32,
    size: u32,
}

impl CabooseReader {
    pub fn new(region: core::ops::Range<u32>) -> Result<Self, CabooseError> {
        if region.is_empty() {
            Err(CabooseError::MissingCaboose)
        } else {
            Ok(Self {
                base: region.start,
                size: region.len() as u32,
            })
        }
    }

    /// Looks up the given key
    pub fn get(&self, key: [u8; 4]) -> Result<&'static [u8], CabooseError> {
        let mut reader = TlvcReader::begin(*self)
            .map_err(|_| CabooseError::TlvcReaderBeginFailed)?;
        while let Ok(Some(chunk)) = reader.next() {
            if chunk.header().tag == key {
                let mut tmp = [0u8; 32];
                if chunk.check_body_checksum(&mut tmp).is_err() {
                    return Err(CabooseError::BadChecksum);
                }
                // At this point, the reader is positioned **after** the data
                // from the target chunk.  We'll back up to the start of the
                // data slice.
                let (_reader, pos, _end) = reader.into_inner();

                let pos = pos as u32;
                let data_len = chunk.header().len.get();

                let data_start = pos
                    - chunk.header().total_len_in_bytes() as u32
                    + core::mem::size_of::<tlvc::ChunkHeader>() as u32;

                // SAFETY:
                // The TLV-C reader guarantees that this chunk does not extend
                // past the end of the medium.  The region is in flash, so no
                // one should be making mutable references to it.
                let slice = unsafe {
                    core::slice::from_raw_parts(
                        (self.base + data_start) as *const u8,
                        data_len as usize,
                    )
                };
                return Ok(slice);
            }
        }

        Err(CabooseError::NoSuchTag)
    }
}

impl TlvcRead for CabooseReader {
    fn extent(&self) -> Result<u64, TlvcReadError> {
        Ok(self.size as u64)
    }

    fn read_exact(
        &self,
        offset: u64,
        dest: &mut [u8],
    ) -> Result<(), TlvcReadError> {
        let addr: u32 = self.base + u32::try_from(offset).unwrap_lite();
        for (i, out) in dest.iter_mut().enumerate() {
            *out = unsafe { *((addr as usize + i) as *const u8) };
        }
        Ok(())
    }
}
