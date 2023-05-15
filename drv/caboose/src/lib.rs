// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the caboose reader task

#![no_std]

use derive_idol_err::IdolError;
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use userlib::FromPrimitive;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum CabooseError {
    MissingCaboose = 1,
    TlvcReaderBeginFailed,
    TlvcReadExactFailed,
    NoSuchTag,
    BadChecksum,
    NoImageHeader,
    RawReadFailed,
    InvalidRead,
}

/// Simple handle which points to the beginning of the TLV-C region of the
/// caboose and allows us to implement `TlvcRead`
#[derive(Copy, Clone)]
pub struct CabooseReader<'a>(&'a [u8]);

impl<'a> CabooseReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self(data)
    }
    /// Looks up the given key
    pub fn get(&self, key: [u8; 4]) -> Result<&'a [u8], CabooseError> {
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

                // The TLV-C reader guarantees that this chunk does not extend
                // past the end of the medium, so making this slice should never
                // panic.
                return Ok(&self.0[data_start as usize..][..data_len as usize]);
            }
        }

        Err(CabooseError::NoSuchTag)
    }
}

impl TlvcRead for CabooseReader<'_> {
    type Error = core::convert::Infallible;

    fn extent(&self) -> Result<u64, TlvcReadError<Self::Error>> {
        Ok(self.0.len() as u64)
    }

    fn read_exact(
        &self,
        offset: u64,
        dest: &mut [u8],
    ) -> Result<(), TlvcReadError<Self::Error>> {
        dest.copy_from_slice(&self.0[offset as usize..][..dest.len()]);
        Ok(())
    }
}
