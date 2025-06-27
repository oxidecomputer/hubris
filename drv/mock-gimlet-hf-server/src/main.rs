// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Gimlet host flash server.
//!
//! This server is responsible for managing access to the host flash; it embeds
//! the QSPI flash driver.

#![no_std]
#![no_main]

use drv_hash_api::SHA256_SZ;
use drv_hf_api::{
    HfChipId, HfDevSelect, HfError, HfMuxState, HfPersistentData,
    HfProtectMode, PAGE_SIZE_BYTES,
};
use idol_runtime::{
    ClientError, Leased, LenLimit, NotificationHandler, RequestError, R, W,
};
use userlib::{RecvMessage, UnwrapLite};

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        capacity: 16 << 20, // Claim to have 16 MiB
        mux_state: HfMuxState::SP,
        dev_state: HfDevSelect::Flash0,
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    capacity: usize,

    /// Selects between the SP and SP3 talking to the QSPI flash
    mux_state: HfMuxState,

    /// Selects between QSPI flash chips 1 and 2 (if present)
    dev_state: HfDevSelect,
}

impl idl::InOrderHostFlashImpl for ServerImpl {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfChipId, RequestError<HfError>> {
        Ok(HfChipId {
            mfr_id: 0,
            memory_type: 1,
            capacity: 2,
            unique_id: *b"mockmockmockmock!",
        })
    }

    fn capacity(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<HfError>> {
        Ok(self.capacity)
    }

    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<HfError>> {
        Ok(0)
    }

    fn bulk_erase(
        &mut self,
        _: &RecvMessage,
        _protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        Ok(())
    }

    fn page_program(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        _protect: HfProtectMode,

        _data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        Ok(())
    }

    fn page_program_dev(
        &mut self,
        msg: &RecvMessage,
        dev: HfDevSelect,
        addr: u32,
        protect: HfProtectMode,
        data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        let prev = self.dev_state;
        self.set_dev(msg, dev)?;
        let r = self.page_program(msg, addr, protect, data);
        self.set_dev(msg, prev).unwrap_lite();
        r
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        let zero = [0; PAGE_SIZE_BYTES];

        dest.write_range(0..dest.len(), &zero[..dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        Ok(())
    }

    fn read_dev(
        &mut self,
        msg: &RecvMessage,
        dev: HfDevSelect,
        addr: u32,
        dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        let prev = self.dev_state;
        self.set_dev(msg, dev)?;
        let r = self.read(msg, addr, dest);
        self.set_dev(msg, prev).unwrap_lite();
        r
    }

    fn sector_erase(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        _protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        Ok(())
    }

    fn sector_erase_dev(
        &mut self,
        msg: &RecvMessage,
        dev: HfDevSelect,
        addr: u32,
        protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        let prev = self.dev_state;
        self.set_dev(msg, dev)?;
        let r = self.sector_erase(msg, addr, protect);
        self.set_dev(msg, prev).unwrap();
        r
    }

    fn get_mux(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfMuxState, RequestError<HfError>> {
        Ok(self.mux_state)
    }

    fn set_mux(
        &mut self,
        _: &RecvMessage,
        state: HfMuxState,
    ) -> Result<(), RequestError<HfError>> {
        self.mux_state = state;
        Ok(())
    }

    fn get_dev(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfDevSelect, RequestError<HfError>> {
        Ok(self.dev_state)
    }

    fn set_dev(
        &mut self,
        _: &RecvMessage,
        state: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        self.dev_state = state;
        Ok(())
    }

    fn check_dev(
        &mut self,
        _: &RecvMessage,
        _state: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        match self.mux_state {
            HfMuxState::SP => Ok(()),
            HfMuxState::HostCPU => Err(HfError::NotMuxedToSP.into()),
        }
    }

    fn hash(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        _len: u32,
    ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
        Ok(*b"mockmockmockmockmockmockmockmock")
    }

    fn hash_significant_bits(
        &mut self,
        _: &RecvMessage,
        _slot: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        Ok(())
    }

    fn get_cached_hash(
        &mut self,
        _: &RecvMessage,
        _slot: HfDevSelect,
    ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
        Ok(*b"mockmockmockmockmockmockmockmock")
    }

    fn write_persistent_data(
        &mut self,
        _: &RecvMessage,
        _: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        Ok(())
    }

    fn get_persistent_data(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfPersistentData, RequestError<HfError>> {
        Err(HfError::HashNotConfigured.into())
    }

    fn bonus_page_program(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        _data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        Err(HfError::BadAddress.into())
    }

    fn bonus_read(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        _dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        Err(HfError::BadAddress.into())
    }

    fn bonus_sector_erase(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
    ) -> Result<(), RequestError<HfError>> {
        Err(HfError::BadAddress.into())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}
mod idl {
    use super::{
        HfDevSelect, HfError, HfMuxState, HfPersistentData, HfProtectMode,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
