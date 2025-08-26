// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_hash_api::SHA256_SZ;
use drv_hf_api::{
    HashData, HashState, HfChipId, HfDevSelect, HfError, HfMuxState,
    HfPersistentData, HfProtectMode, HfRawPersistentData, SlotHash,
    HF_PERSISTENT_DATA_STRIDE,
};
use idol_runtime::{
    LeaseBufReader, LeaseBufWriter, Leased, LenLimit, NotificationHandler,
    RequestError, R, W,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use userlib::{set_timer_relative, task_slot, RecvMessage, UnwrapLite};
use zerocopy::{FromZeros, IntoBytes};

use crate::{
    FlashAddr, FlashDriver, Trace, PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES,
};

task_slot!(HASH, hash_driver);

/// We break the 128 MiB flash chip into 2x 32 MiB slots, to match Gimlet
///
/// The upper 64 MiB are used for Bonus Data.
const SLOT_SIZE_BYTES: u32 = 1024 * 1024 * 32;
const BONUS_SIZE_BYTES: u32 = 1024 * 1024 * 64;

pub struct ServerImpl {
    pub drv: FlashDriver,
    pub dev: HfDevSelect,
    hash: HashData,
}

/// This tunes how many bytes we hash in a single async timer notification
/// call. Making this bigger has a significant impact on hash speed at the
/// cost of blocking the SP. Sector size has been a reasonable setting.
const BLOCK_STEP_SIZE: usize = drv_hf_api::SECTOR_SIZE_BYTES;

impl ServerImpl {
    /// Construct a new `ServerImpl`, with side effects
    ///
    /// The SP / host virtual mux is configured to select the SP.
    ///
    /// Persistent data is loaded from the flash chip and used to select `dev`;
    /// in addition, it is made redundant (written to both virtual devices).
    pub fn new(drv: FlashDriver) -> Self {
        let mut out = Self {
            dev: drv_hf_api::HfDevSelect::Flash0,
            drv,
            hash: HashData::new(HASH.get_task_id()),
        };
        out.drv.set_flash_mux_state(HfMuxState::SP);
        out.ensure_persistent_data_is_redundant();
        if let Ok(p) = out.get_persistent_data() {
            out.dev = p.dev_select;
        }
        out.drv.set_espi_addr_offset(out.flash_base());
        out
    }

    /// Checks whether the given (relative) address is writable
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

    /// Returns the current device's absolute base address
    fn flash_base(&self) -> FlashAddr {
        // This is always valid, so we can unwrap it here
        FlashAddr::new(Self::flash_base_for(self.dev)).unwrap_lite()
    }

    /// Converts a relative address to an absolute address in out current device
    fn flash_addr(&self, offset: u32, size: u32) -> Result<FlashAddr, HfError> {
        if offset
            .checked_add(size)
            .is_some_and(|a| a <= SLOT_SIZE_BYTES)
        {
            Self::flash_addr_for(offset, self.dev)
        } else {
            Err(HfError::BadAddress)
        }
    }

    /// Converts a relative address to an absolute address in bonus space
    fn bonus_addr(offset: u32, len: u32) -> Result<FlashAddr, HfError> {
        if offset
            .checked_add(len)
            .is_some_and(|a| a <= BONUS_SIZE_BYTES)
        {
            let addr = offset
                .checked_add(2 * SLOT_SIZE_BYTES)
                .ok_or(HfError::BadAddress)?;
            Ok(FlashAddr(addr))
        } else {
            Err(HfError::BadAddress)
        }
    }

    /// Converts a relative address to an absolute address in a device slot
    fn flash_addr_for(
        offset: u32,
        dev: HfDevSelect,
    ) -> Result<FlashAddr, HfError> {
        let addr = offset
            .checked_add(Self::flash_base_for(dev))
            .ok_or(HfError::BadAddress)?;
        FlashAddr::new(addr).ok_or(HfError::BadAddress)
    }

    /// Return the absolute flash address base for the given virtual device
    fn flash_base_for(dev: HfDevSelect) -> u32 {
        match dev {
            HfDevSelect::Flash0 => 0,
            HfDevSelect::Flash1 => SLOT_SIZE_BYTES,
        }
    }

    /// Scans the provided (virtual) device for persistent data
    ///
    /// Returns a tuple containing
    /// - newest valid persistent data record
    /// - address of first empty slot
    fn persistent_data_scan(
        &mut self,
        dev: HfDevSelect,
    ) -> (Option<HfRawPersistentData>, Option<u32>) {
        let mut best: Option<HfRawPersistentData> = None;
        let mut empty_slot: Option<u32> = None;
        for i in 0..SECTOR_SIZE_BYTES / HF_PERSISTENT_DATA_STRIDE as u32 {
            let addr = i * HF_PERSISTENT_DATA_STRIDE as u32;
            let mut data = HfRawPersistentData::new_zeroed();
            self.drv
                .flash_read(
                    Self::flash_addr_for(addr, dev).unwrap_lite(),
                    &mut data.as_mut_bytes(),
                )
                .unwrap_lite(); // flash_read is infallible when using a slice
            best = best.max(Some(data).filter(|d| d.is_valid()));
            if empty_slot.is_none()
                && data.as_bytes().iter().all(|b| *b == 0xFF)
            {
                empty_slot = Some(addr);
            }
        }
        (best, empty_slot)
    }

    fn get_persistent_data(&mut self) -> Result<HfPersistentData, HfError> {
        self.get_raw_persistent_data().map(|out| HfPersistentData {
            dev_select: match out.dev_select {
                0 => HfDevSelect::Flash0,
                1 => HfDevSelect::Flash1,

                // get_raw_persistent_data only returns data with a valid
                // HfDevSelect. TODO add an intermediate type for a validated
                // HfRawPersistentData?
                _ => unreachable!(),
            },
        })
    }

    /// Reads persistent data from both slots, returning the newest
    fn get_raw_persistent_data(
        &mut self,
    ) -> Result<HfRawPersistentData, HfError> {
        self.drv.check_flash_mux_state()?;

        // Read the two slots
        let (data0, _) = self.persistent_data_scan(HfDevSelect::Flash0);
        let (data1, _) = self.persistent_data_scan(HfDevSelect::Flash1);

        data0.max(data1).ok_or(HfError::NoPersistentData)
    }

    /// Writes raw persistent data to the given address on the
    /// currently-selected virtual flash device.
    ///
    /// If `addr` is `None`, then we're out of available space; erase all of
    /// sector 0 and write to address 0 upon success.
    ///
    /// # Panics
    /// If `addr` points outside the slot
    fn write_raw_persistent_data_to_addr(
        &mut self,
        addr: Option<u32>,
        raw_data: &HfRawPersistentData,
        dev: HfDevSelect,
    ) {
        let addr = match addr {
            Some(a) => Self::flash_addr_for(a, dev).unwrap_lite(),
            None => {
                let addr = Self::flash_addr_for(0, dev).unwrap_lite();
                self.drv.flash_sector_erase(addr);
                addr
            }
        };
        // flash_write is infallible when given a slice
        self.drv
            .flash_write(addr, &mut raw_data.as_bytes())
            .unwrap_lite();
    }

    /// Ensures that the persistent data is consistent between the virtual devs
    fn ensure_persistent_data_is_redundant(&mut self) {
        // This should only be called on startup, at which point we're always
        // muxed to the SP.
        self.drv.check_flash_mux_state().unwrap_lite();

        // Load the current state of persistent data from flash
        let (data0, next0) = self.persistent_data_scan(HfDevSelect::Flash0);
        let (data1, next1) = self.persistent_data_scan(HfDevSelect::Flash1);

        // The unwrap_lite() are safe because we pick the larger of the two data
        // values, which must be `Some(..)`; if both were `None`, then we'll hit
        // the `Equal` branch below.
        match data0.cmp(&data1) {
            core::cmp::Ordering::Less => {
                self.write_raw_persistent_data_to_addr(
                    next0,
                    &data1.unwrap_lite(),
                    HfDevSelect::Flash0,
                );
            }
            core::cmp::Ordering::Greater => self
                .write_raw_persistent_data_to_addr(
                    next1,
                    &data0.unwrap_lite(),
                    HfDevSelect::Flash1,
                ),
            core::cmp::Ordering::Equal => {
                // Redundant data is consistent (or both empty)
                // TODO: should we have a special case if they don't agree?
            }
        }
    }

    // This assumes `begin` and `end` have been bounds checked for overflow
    // and against the flash chip bounds.
    fn hash_range_update(
        &mut self,
        dev: HfDevSelect,
        begin: usize,
        end: usize,
    ) -> Result<(), HfError> {
        let mut buf = [0u8; PAGE_SIZE_BYTES];
        for addr in (begin..end).step_by(buf.len()) {
            let size = (end - addr).min(buf.len());
            // This unwrap is safe because `flash_read` can only fail when given
            // a lease (where writing into the lease fails if the client goes
            // away).  Giving it a buffer is infallible.
            self.drv
                .flash_read(
                    // We expect that begin and end have already been
                    // bounds checked so this should never fail.
                    Self::flash_addr_for(addr as u32, dev).unwrap_lite(),
                    &mut &mut buf[..size],
                )
                .unwrap_lite();
            if let Err(e) = self.hash.task.update(size as u32, &buf[..size]) {
                ringbuf_entry!(Trace::HashUpdateError(e));
                return Err(HfError::HashError);
            }
        }

        Ok(())
    }

    fn step_hash(&mut self) {
        match self.hash.state {
            HashState::Hashing { dev, addr, end } => {
                let step_size = BLOCK_STEP_SIZE;

                let prev = self.dev;
                self.set_dev(dev).unwrap();
                // The only way we should get an error from this is if
                // we somehow call update before we've initialized or
                // after we've finished the hash.
                self.hash_range_update(dev, addr, addr + step_size)
                    .unwrap_lite();
                self.set_dev(prev).unwrap(); // infallible if the earlier set_dev worked

                if addr + step_size >= end {
                    self.hash.state = HashState::Done;
                    match self.hash.task.finalize_sha256() {
                        Ok(v) => match dev {
                            HfDevSelect::Flash0 => {
                                self.hash.cached_hash0 = SlotHash::Hash(v);
                            }
                            HfDevSelect::Flash1 => {
                                self.hash.cached_hash1 = SlotHash::Hash(v);
                            }
                        },
                        Err(e) => {
                            ringbuf_entry!(Trace::HashUpdateError(e));
                        }
                    };
                } else {
                    self.hash.state = HashState::Hashing {
                        dev,
                        addr: addr + step_size,
                        end,
                    };
                    set_timer_relative(1, notifications::TIMER_MASK);
                };
            }
            // We could potentially end up here if we miss a
            // timer notification on update
            _ => (),
        }
    }

    // Write to flash invalidates the hash of current device
    fn invalidate_write(&mut self) {
        match self.hash.state {
            // Only stop the hash for our current device
            HashState::Hashing { dev, .. } => {
                if dev == self.dev {
                    self.hash.state = HashState::NotRunning;
                }
            }
            _ => (),
        }
        match self.dev {
            HfDevSelect::Flash0 => {
                self.hash.cached_hash0 = SlotHash::Recalculate;
            }
            HfDevSelect::Flash1 => {
                self.hash.cached_hash1 = SlotHash::Recalculate;
            }
        }
    }

    // We switched our mux, recalculate everything
    fn invalidate_mux_switch(&mut self) {
        self.hash.state = HashState::NotRunning;
        self.hash.cached_hash0 = SlotHash::Recalculate;
        self.hash.cached_hash1 = SlotHash::Recalculate;
    }

    fn set_dev(
        &mut self,
        dev: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        self.drv.check_flash_mux_state()?;
        self.dev = dev;
        self.drv.set_espi_addr_offset(self.flash_base());
        Ok(())
    }
}

impl idl::InOrderHostFlashImpl for ServerImpl {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfChipId, RequestError<HfError>> {
        self.drv.check_flash_mux_state()?;
        Ok(self.drv.flash_read_id())
    }

    /// Returns the capacity of each host flash slot
    ///
    /// Note that this **is not** the total flash capacity; it's part of the
    /// `HostFlash` API, so we're pretending to be two distinct flash chips,
    /// each with a capacity of 32 MiB.
    fn capacity(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<HfError>> {
        Ok(0x2000000) // 32 MiB
    }

    /// Reads the STATUS_1 register from the SPI flash
    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<HfError>> {
        self.drv.check_flash_mux_state()?;
        Ok(self.drv.read_flash_status())
    }

    /// Erases the currently selected `dev`
    fn bulk_erase(
        &mut self,
        _: &RecvMessage,
        protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        if !matches!(protect, HfProtectMode::AllowModificationsToSector0) {
            return Err(HfError::Sector0IsReserved.into());
        }
        self.drv.check_flash_mux_state()?;
        self.invalidate_write();
        // Don't use the bulk erase command, because it will erase the entire
        // chip.  Instead, use the sector erase to erase the currently-active
        // virtual device.
        for offset in (0..SLOT_SIZE_BYTES).step_by(SECTOR_SIZE_BYTES as usize) {
            self.drv.flash_sector_erase(
                self.flash_addr(offset, SECTOR_SIZE_BYTES)?,
            );
        }
        Ok(())
    }

    /// Writes a page to the currently selected `dev`
    fn page_program(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        protect: HfProtectMode,
        data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        self.check_addr_writable(addr, protect)?;
        self.drv.check_flash_mux_state()?;
        let addr = self.flash_addr(addr, data.len() as u32)?;
        self.invalidate_write();
        self.drv
            .flash_write(
                addr,
                &mut LeaseBufReader::<_, 32>::from(data.into_inner()),
            )
            .map_err(|()| RequestError::went_away())
    }

    fn page_program_dev(
        &mut self,
        msg: &RecvMessage,
        dev: HfDevSelect,
        addr: u32,
        protect: HfProtectMode,
        data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        let prev = self.dev;
        self.set_dev(dev)?; // makes subsequent set_dev infallible
        let r = self.page_program(msg, addr, protect, data);
        self.set_dev(prev).unwrap_lite(); // this is infallible!
        r
    }

    /// Reads a page from the currently selected `dev`
    fn read(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        self.drv.check_flash_mux_state()?;
        self.drv
            .flash_read(
                self.flash_addr(addr, dest.len() as u32)?,
                &mut LeaseBufWriter::<_, 32>::from(dest.into_inner()),
            )
            .map_err(|_| RequestError::went_away())
    }

    /// Reads a page from the specified `dev` and then swaps it back at the end
    fn read_dev(
        &mut self,
        msg: &RecvMessage,
        dev: HfDevSelect,
        addr: u32,
        dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        let prev = self.dev;
        self.set_dev(dev)?; // makes subsequent set_dev infallible
        let r = self.read(msg, addr, dest);
        self.set_dev(prev).unwrap_lite(); // this is infallible!
        r
    }

    /// Erases the 64 KiB sector in the selected `dev` containing the given
    /// address
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
        self.drv.check_flash_mux_state()?;
        self.check_addr_writable(addr, protect)?;
        self.invalidate_write();
        self.drv.flash_sector_erase(self.flash_addr(addr, 0)?);
        Ok(())
    }

    fn sector_erase_dev(
        &mut self,
        msg: &RecvMessage,
        dev: HfDevSelect,
        addr: u32,
        protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        let prev = self.dev;
        self.set_dev(dev)?; // makes subsequent set_dev infallible
        let r = self.sector_erase(msg, addr, protect);
        self.set_dev(prev).unwrap_lite(); // this is infallible!
        r
    }

    /// Writes a page to the bonus region of flash
    fn bonus_page_program(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        // TODO check mux state?
        self.drv
            .flash_write(
                Self::bonus_addr(addr, data.len() as u32)?,
                &mut LeaseBufReader::<_, 32>::from(data.into_inner()),
            )
            .map_err(|()| RequestError::went_away())
    }

    /// Reads a page from the bonus region of flash
    fn bonus_read(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        // TODO check mux state?
        self.drv
            .flash_read(
                Self::bonus_addr(addr, dest.len() as u32)?,
                &mut LeaseBufWriter::<_, 32>::from(dest.into_inner()),
            )
            .map_err(|_| RequestError::went_away())
    }

    /// Erases a 64 KiB sector in the bonus flash device
    fn bonus_sector_erase(
        &mut self,
        _: &RecvMessage,
        addr: u32,
    ) -> Result<(), RequestError<HfError>> {
        // TODO check mux state?
        self.drv.flash_sector_erase(Self::bonus_addr(addr, 0)?);
        Ok(())
    }

    fn get_mux(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfMuxState, RequestError<HfError>> {
        Ok(self.drv.get_flash_mux_state())
    }

    fn set_mux(
        &mut self,
        _: &RecvMessage,
        state: HfMuxState,
    ) -> Result<(), RequestError<HfError>> {
        self.drv.set_flash_mux_state(state);
        self.invalidate_mux_switch();
        Ok(())
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
        self.set_dev(dev)
    }

    fn check_dev(
        &mut self,
        _: &RecvMessage,
        _state: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        self.drv.check_flash_mux_state()?;
        Ok(())
    }

    /// Hashes a region in the currently selected `dev`
    fn hash(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        len: u32,
    ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
        self.drv.check_flash_mux_state()?;

        // Need to check hash state before doing anything else
        // that might mess up the hash in progress
        match self.hash.state {
            HashState::Hashing { .. } => {
                return Err(HfError::HashInProgress.into())
            }
            _ => (),
        }

        if let Err(e) = self.hash.task.init_sha256() {
            ringbuf_entry!(Trace::HashInitError(e));
            return Err(HfError::HashError.into());
        }

        // Check that the hash range is valid.  We **do not** pass the resulting
        // value to `hash_range_update`, which expects relative offsets!
        let _check = self.flash_addr(addr, len)?;
        self.hash_range_update(
            self.dev,
            addr as usize,
            addr as usize + len as usize,
        )?;

        match self.hash.task.finalize_sha256() {
            Ok(sum) => Ok(sum),
            Err(e) => {
                ringbuf_entry!(Trace::HashFinalizeError(e));
                Err(HfError::HashError.into())
            }
        }
    }

    /// This starts a sha256 on the entire range _except_ sector0
    /// which is treated as `0xff`. We don't hash sector0 because
    /// that's where we store our persistent information and this
    /// function is used to check against files.
    fn hash_significant_bits(
        &mut self,
        _: &RecvMessage,
        dev: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        self.drv.check_flash_mux_state()?;

        // Need to check hash state before doing anything else
        // that might mess up the hash in progress
        match self.hash.state {
            HashState::Hashing { .. } => {
                return Err(HfError::HashInProgress.into())
            }
            _ => (),
        }

        if self.hash.task.init_sha256().is_err() {
            return Err(HfError::HashError.into());
        }

        // If we already have a valid hash for the slot don't bother
        // starting again
        match dev {
            HfDevSelect::Flash0 => match self.hash.cached_hash0 {
                SlotHash::Hash { .. } => return Ok(()),
                _ => {
                    self.hash.cached_hash0 = SlotHash::HashInProgress;
                }
            },
            HfDevSelect::Flash1 => match self.hash.cached_hash1 {
                SlotHash::Hash { .. } => return Ok(()),
                _ => {
                    self.hash.cached_hash1 = SlotHash::HashInProgress;
                }
            },
        }

        // Treat sector 0 as all `0xff`
        let mut buf = [0u8; PAGE_SIZE_BYTES];
        buf.fill(0xff);
        for _ in (0..SECTOR_SIZE_BYTES).step_by(buf.len()) {
            self.hash
                .task
                .update(buf.len() as u32, &buf)
                .map_err(|_| RequestError::Runtime(HfError::HashError))?;
        }

        self.hash.state = HashState::Hashing {
            dev,
            addr: drv_hf_api::SECTOR_SIZE_BYTES,
            end: SLOT_SIZE_BYTES as usize,
        };
        set_timer_relative(1, notifications::TIMER_MASK);
        Ok(())
    }

    fn get_cached_hash(
        &mut self,
        _: &RecvMessage,
        dev: HfDevSelect,
    ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
        match dev {
            HfDevSelect::Flash0 => {
                self.hash.cached_hash0.get_hash().map_err(|e| e.into())
            }
            HfDevSelect::Flash1 => {
                self.hash.cached_hash1.get_hash().map_err(|e| e.into())
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
        self.drv.check_flash_mux_state()?;

        // Scan both slots for persistent data
        let (data0, next0) = self.persistent_data_scan(HfDevSelect::Flash0);
        let (data1, next1) = self.persistent_data_scan(HfDevSelect::Flash1);

        let prev_monotonic_counter =
            data0.max(data1).map(|d| d.monotonic_counter).unwrap_or(0);

        // Early exit if the previous persistent data matches
        let prev_raw = HfRawPersistentData::new(data, prev_monotonic_counter);
        if data0 == data1 && data0 == Some(prev_raw) {
            return Ok(());
        }

        let monotonic_counter = prev_monotonic_counter
            .checked_add(1)
            .ok_or(HfError::MonotonicCounterOverflow)?;
        let raw = HfRawPersistentData::new(data, monotonic_counter);

        // Write the persistent data to both flash slots
        self.write_raw_persistent_data_to_addr(
            next0,
            &raw,
            HfDevSelect::Flash0,
        );
        self.write_raw_persistent_data_to_addr(
            next1,
            &raw,
            HfDevSelect::Flash1,
        );

        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if (bits & notifications::TIMER_MASK) != 0 {
            self.step_hash();
        }
    }
}

pub mod idl {
    use drv_hf_api::{
        HfChipId, HfDevSelect, HfError, HfMuxState, HfPersistentData,
        HfProtectMode,
    };
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
