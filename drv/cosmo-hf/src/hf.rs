// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_hash_api::SHA256_SZ;
use drv_hf_api::{
    HfDevSelect, HfError, HfMuxState, HfPersistentData, HfProtectMode,
};
use idol_runtime::{
    LeaseBufReader, LeaseBufWriter, Leased, LenLimit, NotificationHandler,
    RequestError, R, W,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use userlib::{task_slot, RecvMessage, UnwrapLite};

use crate::{ServerImpl, Trace, PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES};

task_slot!(HASH, hash_driver);

impl ServerImpl {
    fn check_addr_writable(
        &self,
        addr: u32,
        protect: HfProtectMode,
    ) -> Result<(), HfError> {
        if addr < SECTOR_SIZE_BYTES
            && !matches!(protect, HfProtectMode::AllowModificationsToSector0)
        {
            Err(HfError::Sector0IsReserved.into())
        } else {
            Ok(())
        }
    }
}

impl idl::InOrderHostFlashImpl for ServerImpl {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 20], RequestError<HfError>> {
        Ok(self.flash_read_id())
    }

    fn capacity(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<HfError>> {
        todo!()
    }

    /// Reads the STATUS_1 register from the SPI flash
    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<HfError>> {
        Ok(self.read_flash_status())
    }

    fn bulk_erase(
        &mut self,
        _: &RecvMessage,
        protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        if !matches!(protect, HfProtectMode::AllowModificationsToSector0) {
            return Err(HfError::Sector0IsReserved.into());
        }
        todo!()
    }

    fn page_program(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        protect: HfProtectMode,
        data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        self.check_addr_writable(addr, protect)?;
        self.flash_write(
            addr,
            &mut LeaseBufReader::<_, 32>::from(data.into_inner()),
        )
        .map_err(|()| RequestError::went_away())
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        offset: u32,
        dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        self.flash_read(
            offset,
            &mut LeaseBufWriter::<_, 32>::from(dest.into_inner()),
        )
        .map_err(|_| RequestError::went_away())
    }

    /// Erases the 64 KiB sector containing the given address
    ///
    /// If this is sector 0, requires `protect` to be
    /// `HfProtectMode::AllowModificationsToSector0` (otherwise this function
    /// will return an error).
    fn sector_erase(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        self.check_addr_writable(addr, protect)?;
        self.flash_sector_erase(addr);
        Ok(())
    }

    fn get_mux(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfMuxState, RequestError<HfError>> {
        todo!()
    }

    fn set_mux(
        &mut self,
        _: &RecvMessage,
        _state: HfMuxState,
    ) -> Result<(), RequestError<HfError>> {
        todo!()
    }

    fn get_dev(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfDevSelect, RequestError<HfError>> {
        todo!()
    }

    fn set_dev(
        &mut self,
        _: &RecvMessage,
        _state: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        todo!()
    }

    fn hash(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        len: u32,
    ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
        let hash_driver = drv_hash_api::Hash::from(HASH.get_task_id());
        if hash_driver.init_sha256().is_err() {
            return Err(HfError::HashError.into());
        }
        let begin = addr as usize;
        // TODO: Begin may be an address beyond physical end of
        // flash part and may wrap around.
        let end = begin
            .checked_add(len as usize)
            .ok_or(HfError::HashBadRange)?;

        let mut buf = [0u8; PAGE_SIZE_BYTES];
        for addr in (begin..end).step_by(buf.len()) {
            let size = (end - addr).min(buf.len());
            // This unwrap is safe because `flash_read` can only fail when given
            // a lease (where writing into the lease fails if the client goes
            // away).  Giving it a buffer is infallible.
            self.flash_read(addr as u32, &mut &mut buf[..size])
                .unwrap_lite();
            if let Err(e) = hash_driver.update(size as u32, &buf[..size]) {
                ringbuf_entry!(Trace::HashUpdateError(e));
                return Err(HfError::HashError.into());
            }
        }
        match hash_driver.finalize_sha256() {
            Ok(sum) => Ok(sum),
            Err(e) => {
                ringbuf_entry!(Trace::HashFinalizeError(e));
                Err(HfError::HashError.into())
            }
        }
    }

    fn get_persistent_data(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfPersistentData, RequestError<HfError>> {
        todo!()
    }

    fn write_persistent_data(
        &mut self,
        _: &RecvMessage,
        _dev_select: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        todo!()
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

pub mod idl {
    use drv_hf_api::{
        HfDevSelect, HfError, HfMuxState, HfPersistentData, HfProtectMode,
    };
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
