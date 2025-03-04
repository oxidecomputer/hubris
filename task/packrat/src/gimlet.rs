// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Gimlet-specific packrat data.

use crate::Trace;
use core::convert::Infallible;
use drv_cpu_seq_api::NUM_SPD_BANKS;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_packrat_api::HostStartupOptions;

const SPD_DATA_LEN: usize =
    NUM_SPD_BANKS * spd::MAX_SIZE * spd::MAX_DEVICES as usize;
const SPD_PRESENT_LEN: usize = NUM_SPD_BANKS * spd::MAX_DEVICES as usize;
static_assertions::const_assert_eq!(SPD_DATA_LEN, 8192);

pub(crate) struct GimletData {
    host_startup_options: &'static mut HostStartupOptions,
    spd_present: &'static mut [bool; SPD_PRESENT_LEN],
    spd_data: &'static mut [u8; SPD_DATA_LEN],
}

const fn default_host_startup_options() -> HostStartupOptions {
    if cfg!(feature = "boot-kmdb") {
        // We have to do this because const fn.
        let bits = HostStartupOptions::STARTUP_KMDB.bits()
            | HostStartupOptions::STARTUP_PROM.bits()
            | HostStartupOptions::STARTUP_VERBOSE.bits();
        match HostStartupOptions::from_bits(bits) {
            Some(options) => options,
            None => panic!("must be valid at compile-time"),
        }
    } else {
        HostStartupOptions::empty()
    }
}

pub(crate) struct StaticBufs {
    spd_present: [bool; SPD_PRESENT_LEN],
    spd_data: [u8; SPD_DATA_LEN],
    host_startup_options: HostStartupOptions,
}

impl StaticBufs {
    pub(crate) const fn new() -> Self {
        Self {
            spd_present: [false; SPD_PRESENT_LEN],
            spd_data: [0; SPD_DATA_LEN],
            host_startup_options: default_host_startup_options(),
        }
    }
}

impl GimletData {
    pub(crate) fn new(
        StaticBufs {
            ref mut host_startup_options,
            ref mut spd_data,
            ref mut spd_present,
        }: &'static mut StaticBufs,
    ) -> Self {
        Self {
            host_startup_options,
            spd_present,
            spd_data,
        }
    }

    pub(crate) fn host_startup_options(&self) -> HostStartupOptions {
        *self.host_startup_options
    }

    pub(crate) fn set_host_startup_options(
        &mut self,
        options: HostStartupOptions,
    ) {
        *self.host_startup_options = options;
    }

    pub(crate) fn set_spd_eeprom(
        &mut self,
        index: u8,
        page1: bool,
        offset: u8,
        data: LenLimit<Leased<idol_runtime::R, [u8]>, 256>,
    ) -> Result<(), RequestError<Infallible>> {
        let eeprom_base = spd::MAX_SIZE * usize::from(index);
        let eeprom_offset =
            spd::PAGE_SIZE * usize::from(page1) + usize::from(offset);

        if eeprom_offset + data.len() > spd::MAX_SIZE {
            return Err(ClientError::BadMessageContents.fail());
        }

        let addr = eeprom_base + eeprom_offset;

        if addr + data.len() > self.spd_data.len() {
            return Err(ClientError::BadMessageContents.fail());
        }

        ringbuf_entry!(Trace::SpdDataUpdate {
            index,
            page1,
            offset,
            len: data.len() as u8,
        });

        // `index` is implicitly in range due to our check in `addr` above;
        // double-check that the compiler realizes it and ellides this panic
        // path.
        self.spd_present[usize::from(index)] = true;

        data.read_range(
            0..data.len(),
            &mut self.spd_data[addr..addr + data.len()],
        )
        .map_err(|()| RequestError::went_away())?;

        Ok(())
    }

    pub(crate) fn get_spd_present(
        &self,
        index: usize,
    ) -> Result<bool, RequestError<Infallible>> {
        self.spd_present
            .get(index)
            .copied()
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))
    }

    pub(crate) fn get_spd_data(
        &self,
        index: usize,
    ) -> Result<u8, RequestError<Infallible>> {
        self.spd_data
            .get(index)
            .copied()
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))
    }

    pub(crate) fn get_full_spd_data(
        &self,
        dev: usize,
        out: LenLimit<Leased<idol_runtime::W, [u8]>, 512>,
    ) -> Result<(), RequestError<Infallible>> {
        if out.len() != spd::MAX_SIZE {
            Err(RequestError::Fail(idol_runtime::ClientError::BadLease))
        } else if let Some(s) = self
            .spd_data
            .get((dev * spd::MAX_SIZE)..((dev + 1) * spd::MAX_SIZE))
        {
            out.write_range(0..spd::MAX_SIZE, s)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            Ok(())
        } else {
            Err(RequestError::Fail(
                idol_runtime::ClientError::BadMessageContents,
            ))
        }
    }
}
