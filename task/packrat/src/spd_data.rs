// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use core::convert::Infallible;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError};
use ringbuf::ringbuf_entry_root as ringbuf_entry;

pub(crate) struct SpdData<const DIMM_COUNT: usize, const DATA_SIZE: usize> {
    spd_present: [bool; DIMM_COUNT],
    spd_data: [[u8; DATA_SIZE]; DIMM_COUNT],
}

impl<const DIMM_COUNT: usize, const DATA_SIZE: usize>
    SpdData<DIMM_COUNT, DATA_SIZE>
{
    #[cfg_attr(not(any(feature = "gimlet", feature = "cosmo")), allow(unused))]
    pub const fn new() -> Self {
        Self {
            spd_present: [false; DIMM_COUNT],
            spd_data: [[0; DATA_SIZE]; DIMM_COUNT],
        }
    }

    pub fn set_eeprom(
        &mut self,
        index: u8,
        offset: usize,
        data: LenLimit<Leased<idol_runtime::R, [u8]>, 256>,
    ) -> Result<(), RequestError<Infallible>> {
        ringbuf_entry!(Trace::SpdDataUpdate {
            index,
            offset,
            len: data.len() as u8,
        });
        if index as usize >= DIMM_COUNT {
            return Err(ClientError::BadMessageContents.fail());
        }
        if offset + data.len() > DATA_SIZE {
            return Err(ClientError::BadMessageContents.fail());
        }

        // `index` is implicitly in range due to our check in `addr` above;
        // double-check that the compiler realizes it and ellides this panic
        // path.
        self.spd_present[usize::from(index)] = true;

        data.read_range(
            0..data.len(),
            &mut self.spd_data[usize::from(index)][offset..offset + data.len()],
        )
        .map_err(|()| RequestError::went_away())?;
        Ok(())
    }

    pub fn remove_eeprom(
        &mut self,
        index: u8,
    ) -> Result<(), RequestError<Infallible>> {
        ringbuf_entry!(Trace::SpdRemoveEeprom { index });
        if index as usize >= DIMM_COUNT {
            return Err(ClientError::BadMessageContents.fail());
        }
        self.spd_present[usize::from(index)] = false;
        Ok(())
    }

    pub fn get_present(
        &self,
        index: u8,
    ) -> Result<bool, RequestError<Infallible>> {
        self.spd_present
            .get(usize::from(index))
            .copied()
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))
    }

    pub fn get_data(
        &self,
        index: u8,
        offset: usize,
    ) -> Result<u8, RequestError<Infallible>> {
        self.spd_data
            .get(usize::from(index))
            .and_then(|d| d.get(offset))
            .copied()
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))
    }

    pub fn get_full_data(
        &self,
        index: u8,
        out: Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), RequestError<Infallible>> {
        if out.len() != DATA_SIZE {
            Err(RequestError::Fail(idol_runtime::ClientError::BadLease))
        } else if let Some(s) = self.spd_data.get(usize::from(index)) {
            out.write_range(0..DATA_SIZE, s)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            Ok(())
        } else {
            Err(RequestError::Fail(
                idol_runtime::ClientError::BadMessageContents,
            ))
        }
    }
}
