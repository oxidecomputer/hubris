// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_hash_api::SHA256_SZ;
use drv_hf_api::{
    HfDevSelect, HfError, HfMuxState, HfPersistentData, HfProtectMode,
    HfRawPersistentData, HF_PERSISTENT_DATA_STRIDE,
};
use idol_runtime::{
    LeaseBufReader, LeaseBufWriter, Leased, LenLimit, NotificationHandler,
    RequestError, R, W,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use userlib::{task_slot, RecvMessage, UnwrapLite};
use zerocopy::{AsBytes, FromBytes};

use crate::{FlashDriver, Trace, PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES};

task_slot!(HASH, hash_driver);

/// We break the 128 MiB flash chip into 2x 32 MiB slots, to match Gimlet
///
/// The upper 64 MiB are unused (which is good, because it's a separate die and
/// requires special handling).
const SLOT_SIZE_BYTES: u32 = 1024 * 1024 * 32;

pub struct ServerImpl {
    pub drv: FlashDriver,
    pub dev: HfDevSelect,
}

impl ServerImpl {
    /// Construct a new `ServerImpl`, with side effects
    ///
    /// Persistent data is loaded from the flash chip and used to select `dev`;
    /// in addition, it is made reduntant (written to both virtual devices).
    pub fn new(drv: FlashDriver) -> Self {
        let mut out = Self {
            dev: drv_hf_api::HfDevSelect::Flash0,
            drv,
        };
        out.ensure_persistent_data_is_redundant();
        if let Ok(p) = out.get_persistent_data() {
            out.dev = p.dev_select;
        }
        out
    }

    fn check_addr_writable(
        &self,
        addr: u32,
        protect: HfProtectMode,
    ) -> Result<(), HfError> {
        if addr < SECTOR_SIZE_BYTES
            && !matches!(protect, HfProtectMode::AllowModificationsToSector0)
        {
            Err(HfError::Sector0IsReserved)
        } else {
            Ok(())
        }
    }

    fn flash_base(&self) -> u32 {
        match self.dev {
            HfDevSelect::Flash0 => 0,
            HfDevSelect::Flash1 => SLOT_SIZE_BYTES,
        }
    }

    fn flash_addr(&self, offset: u32) -> u32 {
        offset + self.flash_base()
    }

    /// Scans the currently active (virtual) device for persistent data
    ///
    /// Returns a tuple containing
    /// - newest persistent data record
    /// - address of first empty slot
    fn persistent_data_scan(
        &mut self,
    ) -> (Option<HfRawPersistentData>, Option<u32>) {
        let mut best: Option<HfRawPersistentData> = None;
        let mut empty_slot: Option<u32> = None;
        for i in 0..SECTOR_SIZE_BYTES / HF_PERSISTENT_DATA_STRIDE as u32 {
            let addr = i * HF_PERSISTENT_DATA_STRIDE as u32;
            let mut data = HfRawPersistentData::new_zeroed();
            self.drv
                .flash_read(self.flash_addr(addr), &mut data.as_bytes_mut())
                .unwrap_lite(); // flash_read is infallible when using a slice
            if data.is_valid() && best.map(|b| data > b).unwrap_or(true) {
                best = Some(data);
            }
            if empty_slot.is_none()
                && data.as_bytes().iter().all(|b| *b == 0xFF)
            {
                empty_slot = Some(addr);
            }
        }
        (best, empty_slot)
    }

    fn get_persistent_data(&mut self) -> Result<HfPersistentData, HfError> {
        let out = self.get_raw_persistent_data()?;
        Ok(HfPersistentData {
            dev_select: match out.dev_select {
                0 => HfDevSelect::Flash0,
                _ => HfDevSelect::Flash1,
            },
        })
    }

    /// Reads persistent data from both slots, returning the newest
    fn get_raw_persistent_data(
        &mut self,
    ) -> Result<HfRawPersistentData, HfError> {
        let prev_slot = self.dev;

        // Look at the inactive slot first
        self.dev = !prev_slot;
        let (a, _) = self.persistent_data_scan();

        // Then switch back to our current slot, so that the resulting state
        // is unchanged.
        self.dev = prev_slot;
        let (b, _) = self.persistent_data_scan();

        match (a, b) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(_), None) => a,
            (None, Some(_)) => b,
            (None, None) => None,
        }
        .ok_or(HfError::NoPersistentData)
    }

    /// Writes raw persistent data to the given address on the
    /// currently-selected virtual flash device.
    ///
    /// If `addr` is `None`, then we're out of available space; erase all of
    /// sector 0 and write to address 0 upon success.
    fn write_raw_persistent_data_to_addr(
        &mut self,
        addr: Option<u32>,
        raw_data: &HfRawPersistentData,
    ) {
        let addr = match addr {
            Some(a) => self.flash_addr(a),
            None => {
                let addr = self.flash_addr(0);
                self.drv.flash_sector_erase(addr);
                addr
            }
        };
        // flash_write is infallible when given a slice
        self.drv
            .flash_write(addr, &mut raw_data.as_bytes())
            .unwrap_lite()
    }

    /// Ensures that the persistent data is consistent between the virtual devs
    fn ensure_persistent_data_is_redundant(&mut self) {
        // Load the current state of persistent data from flash
        let prev_slot = self.dev;

        self.dev = !prev_slot;
        let (a_data, a_next) = self.persistent_data_scan();

        self.dev = prev_slot;
        let (b_data, b_next) = self.persistent_data_scan();

        match (a_data, b_data) {
            (Some(a), Some(b)) => {
                match a.cmp(&b) {
                    core::cmp::Ordering::Less => {
                        self.dev = !prev_slot;
                        self.write_raw_persistent_data_to_addr(a_next, &b);
                        self.dev = prev_slot;
                    }
                    core::cmp::Ordering::Greater => {
                        self.write_raw_persistent_data_to_addr(b_next, &a)
                    }
                    core::cmp::Ordering::Equal => {
                        // Redundant data is consistent
                        // TODO: should we have a special case if they don't agree?
                    }
                }
            }
            (Some(a), None) => {
                self.write_raw_persistent_data_to_addr(b_next, &a)
            }
            (None, Some(b)) => {
                self.dev = !prev_slot;
                self.write_raw_persistent_data_to_addr(a_next, &b);
                self.dev = prev_slot;
            }
            (None, None) => {
                // No persistent data recorded; nothing to do here
            }
        }
    }
}

impl idl::InOrderHostFlashImpl for ServerImpl {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 20], RequestError<HfError>> {
        Ok(self.drv.flash_read_id())
    }

    fn capacity(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<HfError>> {
        Ok(0x8000000) // 1 GBit = 128 MiB
    }

    /// Reads the STATUS_1 register from the SPI flash
    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<HfError>> {
        Ok(self.drv.read_flash_status())
    }

    fn bulk_erase(
        &mut self,
        _: &RecvMessage,
        protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        if !matches!(protect, HfProtectMode::AllowModificationsToSector0) {
            return Err(HfError::Sector0IsReserved.into());
        }
        // Don't use the bulk erase command, because it will erase the entire
        // chip.  Instead, use the sector erase to erase the currently-active
        // virtual device.
        for offset in (0..SLOT_SIZE_BYTES).step_by(SECTOR_SIZE_BYTES as usize) {
            self.drv.flash_sector_erase(self.flash_addr(offset));
        }
        Ok(())
    }

    fn page_program(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        protect: HfProtectMode,
        data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        self.check_addr_writable(addr, protect)?;
        self.drv
            .flash_write(
                self.flash_addr(addr),
                &mut LeaseBufReader::<_, 32>::from(data.into_inner()),
            )
            .map_err(|()| RequestError::went_away())
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        self.drv
            .flash_read(
                self.flash_addr(addr),
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
        self.drv.flash_sector_erase(self.flash_addr(addr));
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
        Ok(self.dev)
    }

    fn set_dev(
        &mut self,
        _: &RecvMessage,
        dev: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        self.dev = dev;
        self.drv.set_espi_addr_offset(self.flash_base());
        Ok(())
    }

    fn hash(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        len: u32,
    ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
        let hash_driver = drv_hash_api::Hash::from(HASH.get_task_id());
        if let Err(e) = hash_driver.init_sha256() {
            ringbuf_entry!(Trace::HashInitError(e));
            return Err(HfError::HashError.into());
        }
        let begin = self.flash_addr(addr) as usize;
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
            self.drv
                .flash_read(addr as u32, &mut &mut buf[..size])
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
        self.get_persistent_data().map_err(RequestError::from)
    }

    fn write_persistent_data(
        &mut self,
        _: &RecvMessage,
        dev_select: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        let data = HfPersistentData { dev_select };

        let prev_slot = self.dev;

        // Scan both slots for persistent data
        self.dev = !prev_slot;
        let (a_data, a_next) = self.persistent_data_scan();

        self.dev = prev_slot;
        let (b_data, b_next) = self.persistent_data_scan();

        let prev_monotonic_counter = match (a_data, b_data) {
            (Some(a), Some(b)) => a.monotonic_counter.max(b.monotonic_counter),
            (Some(a), None) => a.monotonic_counter,
            (None, Some(b)) => b.monotonic_counter,
            (None, None) => 0,
        };

        // Early exit if the previous persistent data matches
        let prev_raw = HfRawPersistentData::new(data, prev_monotonic_counter);
        if a_data == b_data && a_data == Some(prev_raw) {
            return Ok(());
        }

        let monotonic_counter = prev_monotonic_counter
            .checked_add(1)
            .ok_or(HfError::MonotonicCounterOverflow)?;
        let raw = HfRawPersistentData::new(data, monotonic_counter);

        // Write the persistent data to the currently inactive flash.
        self.dev = !prev_slot;
        self.write_raw_persistent_data_to_addr(a_next, &raw);

        // Swap back to the currently selected flash
        self.dev = prev_slot;

        // Write the persistent data to the currently active flash
        self.write_raw_persistent_data_to_addr(b_next, &raw);

        Ok(())
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

/// Dummy server which returns an error for every operation
pub struct FailServer {
    pub err: HfError,
}

impl idl::InOrderHostFlashImpl for FailServer {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 20], RequestError<HfError>> {
        Err(self.err.into())
    }

    fn capacity(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<HfError>> {
        Err(self.err.into())
    }

    /// Reads the STATUS_1 register from the SPI flash
    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<HfError>> {
        Err(self.err.into())
    }

    fn bulk_erase(
        &mut self,
        _: &RecvMessage,
        _protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        Err(self.err.into())
    }

    fn page_program(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        _protect: HfProtectMode,
        _data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        Err(self.err.into())
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        _offset: u32,
        _dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        Err(self.err.into())
    }

    fn sector_erase(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        _protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        Err(self.err.into())
    }

    fn get_mux(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfMuxState, RequestError<HfError>> {
        Err(self.err.into())
    }

    fn set_mux(
        &mut self,
        _: &RecvMessage,
        _state: HfMuxState,
    ) -> Result<(), RequestError<HfError>> {
        Err(self.err.into())
    }

    fn get_dev(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfDevSelect, RequestError<HfError>> {
        Err(self.err.into())
    }

    fn set_dev(
        &mut self,
        _: &RecvMessage,
        _state: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        Err(self.err.into())
    }

    fn hash(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        _len: u32,
    ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
        Err(self.err.into())
    }

    fn get_persistent_data(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfPersistentData, RequestError<HfError>> {
        Err(self.err.into())
    }

    fn write_persistent_data(
        &mut self,
        _: &RecvMessage,
        _dev_select: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        Err(self.err.into())
    }
}

impl NotificationHandler for FailServer {
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
