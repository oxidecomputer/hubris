use drv_hash_api::SHA256_SZ;
use drv_hf_api::{
    HfDevSelect, HfError, HfMuxState, HfPersistentData, HfProtectMode,
};
use idol_runtime::{
    LeaseBufReader, LeaseBufWriter, Leased, LenLimit, NotificationHandler,
    RequestError, R, W,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use userlib::{task_slot, RecvMessage};

use crate::{ServerImpl, Trace, PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES};

task_slot!(HASH, hash_driver);

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
        if addr as usize / SECTOR_SIZE_BYTES == 0
            && !matches!(protect, HfProtectMode::AllowModificationsToSector0)
        {
            return Err(HfError::Sector0IsReserved.into());
        }
        self.flash_page_program(
            addr,
            LeaseBufReader::<_, 32>::from(data.into_inner()),
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
            LeaseBufWriter::<_, 32>::from(dest.into_inner()),
        )
        .map_err(|_| RequestError::went_away())
    }

    fn sector_erase(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        if addr as usize / SECTOR_SIZE_BYTES == 0
            && !matches!(protect, HfProtectMode::AllowModificationsToSector0)
        {
            return Err(HfError::Sector0IsReserved.into());
        }
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
        let end = match begin.checked_add(len as usize) {
            Some(end) => {
                // Check end > maximum 4-byte address.
                // TODO: End may be beyond physical end of flash part.
                //       Use that limit rather than maximum 4-byte address.
                if end > u32::MAX as usize {
                    return Err(HfError::HashBadRange.into());
                } else {
                    end
                }
            }
            None => {
                return Err(HfError::HashBadRange.into());
            }
        };
        // If we knew the flash part size, we'd check against those limits.
        let mut buf = [0u8; PAGE_SIZE_BYTES];
        for addr in (begin..end).step_by(buf.len()) {
            let size = (end - addr).min(buf.len());
            self.flash_read(addr as u32, &mut buf[..size]).unwrap();
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
