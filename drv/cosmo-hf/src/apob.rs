// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tools to extract the APOB location from an AMD ROM
//!
//! For details, see AMD document 57299; tables and sections in this code refer
//! to Rev. 2.0 February 2025.

use crate::{
    hf::ServerImpl, FlashAddr, FlashDriver, PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES,
};
use drv_hf_api::{
    ApobBeginError, ApobCommitError, ApobHash, ApobReadError, ApobWriteError,
    HfError,
};
use idol_runtime::{Leased, R, W};
use ringbuf::{counted_ringbuf, ringbuf_entry};
use userlib::UnwrapLite;
use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout};

/// Embedded firmware structure (Table 3)
///
/// Only relevant fields are included here; the EFS extends beyond the size of
/// this `struct`, but we don't care about any subsequent fields.
#[derive(FromBytes, Immutable, IntoBytes)]
#[repr(C)]
pub struct Efs {
    signature: u32,
    _padding1: [u8; 16],
    psp_dir_offset: u32,
    _padding2: [u8; 16],
    bios_dir_offset: u32,
}

const EFS_SIGNATURE: u32 = 0x55aa55aa;
const BHD_DIR_COOKIE: u32 = 0x44484224; // $BHD
const APOB_NV_COPY: u8 = 0x63; // Table 29

/// BIOS Directory Table Header (Table 17)
#[derive(FromBytes, Immutable, IntoBytes)]
#[repr(C)]
pub struct BhdDir {
    cookie: u32,
    checksum: u32,
    num: u32,
    info: u32,
}

/// BIOS Directory Table Entry (Table 18)
#[derive(FromBytes, Immutable, IntoBytes)]
#[repr(C)]
pub struct DirEntry {
    entry_type: u8,
    region_type: u8,
    _unused: [u8; 2], // bitpacked fields
    size: u32,
    src_address: u64, // highest 2 bits are `addr_mode`
    dst_address: u64,
}

impl ServerImpl {
    /// Reads a typed value from the currently selected flash device
    fn read_value<T: FromBytes + Immutable + IntoBytes>(
        &mut self,
        addr: u32,
    ) -> Result<T, HfError> {
        let mut out = T::new_zeroed();
        self.drv
            .flash_read(
                self.flash_addr(addr, core::mem::size_of_val(&out) as u32)?,
                &mut out.as_mut_bytes(),
            )
            .unwrap_lite(); // flash_read is infallible when using a slice
        Ok(out)
    }

    /// Find the APOB location from the currently selected flash device
    pub fn find_apob(&mut self) -> Result<ApobLocation, ApobError> {
        // We expect to find the EFS at offset 0x20000 (§4.1.3)
        let efs: Efs = self.read_value(0x20_000)?;
        if efs.signature != EFS_SIGNATURE {
            return Err(ApobError::BadEfsSignature(efs.signature));
        }

        let bios_dir_offset = efs.bios_dir_offset;
        let bhd: BhdDir = self.read_value(bios_dir_offset)?;
        if bhd.cookie != BHD_DIR_COOKIE {
            return Err(ApobError::BadBhdCookie(bhd.cookie));
        }

        // Directory entries are right after the `BhdDir` header
        let mut pos = bios_dir_offset + core::mem::size_of_val(&bhd) as u32;
        for _ in 0..bhd.num {
            let entry: DirEntry = self.read_value(pos)?;
            if entry.entry_type == APOB_NV_COPY {
                // Mask two `addr_mode` bits
                let src_address = entry.src_address & 0x3FFF_FFFF_FFFF_FFFF;
                let start = src_address
                    .try_into()
                    .map_err(|_| ApobError::AddressIsTooHigh(src_address))?;
                let size = entry.size;

                return Ok(ApobLocation { start, size });
            }
            pos += core::mem::size_of::<DirEntry>() as u32;
        }
        Err(ApobError::NotFound)
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ApobLocation {
    pub start: u32,
    pub size: u32,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ApobError {
    BadEfsSignature(u32),
    BadBhdCookie(u32),
    AddressIsTooHigh(u64),
    NotFound,
    Hf(HfError),
}

impl From<HfError> for ApobError {
    fn from(value: HfError) -> Self {
        Self::Hf(value)
    }
}

////////////////////////////////////////////////////////////////////////////////

pub const APOB_PERSISTENT_DATA_MAGIC: u32 = 0x3ca9_9496; // chosen at random
pub const APOB_PERSISTENT_DATA_STRIDE: usize = 128;
pub const APOB_PERSISTENT_DATA_HEADER_VERSION: u32 = 1;

pub const APOB_META_SIZE: u32 = SECTOR_SIZE_BYTES;
pub const APOB_SLOT_SIZE: u32 = 2 * 1024 * 1024; // 2 MiB (chosen arbitrarily)

// The layout is [meta0, meta1, slot0, slot1]
pub const APOB_META0_ADDR: u32 = crate::hf::SLOT_SIZE_BYTES * 2;
pub const APOB_META1_ADDR: u32 = APOB_META0_ADDR + APOB_META_SIZE;
pub const APOB_SLOT0_ADDR: u32 = APOB_META1_ADDR + APOB_META_SIZE;
pub const APOB_SLOT1_ADDR: u32 = APOB_SLOT0_ADDR + APOB_SLOT_SIZE;

#[derive(Copy, Clone, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    State(#[count(children)] ApobState),
    GotPersistentData {
        #[count(children)]
        meta: Meta,
        data: Option<ApobRawPersistentData>,
    },
    WrotePersistentData {
        #[count(children)]
        meta: Meta,
        data: ApobRawPersistentData,
    },
    HashMismatch {
        expected_hash: [u8; 32],
        actual_hash: [u8; 32],
    },
    ApobSlotErase {
        #[count(children)]
        slot: ApobSlot,
        size: u32,
    },
    ApobSlotEraseDone {
        #[count(children)]
        slot: ApobSlot,
        time_ms: u64,
        num_sectors_erased: usize,
    },
    ApobSlotEraseSkipped {
        #[count(children)]
        slot: ApobSlot,
        time_ms: u64,
    },
    ApobSlotSectorErase {
        #[count(children)]
        slot: ApobSlot,
        offset: u32,
    },
    BadApobSig {
        expected: [u8; 4],
        actual: [u8; 4],
    },
    BadApobVersion {
        expected: u32,
        actual: u32,
    },
    BadApobSize {
        expected: u32,
        actual: u32,
    },
    BadApobWalk {
        expected: u32,
        actual: u32,
    },
}
counted_ringbuf!(Trace, 16, Trace::None);

#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum ApobSlot {
    Slot0,
    Slot1,
}

impl core::ops::Not for ApobSlot {
    type Output = Self;
    fn not(self) -> Self::Output {
        match self {
            ApobSlot::Slot0 => ApobSlot::Slot1,
            ApobSlot::Slot1 => ApobSlot::Slot0,
        }
    }
}

impl ApobSlot {
    pub fn base_addr(&self) -> FlashAddr {
        match self {
            ApobSlot::Slot0 => FlashAddr::new(APOB_SLOT0_ADDR).unwrap(),
            ApobSlot::Slot1 => FlashAddr::new(APOB_SLOT1_ADDR).unwrap(),
        }
    }

    pub fn flash_addr(&self, offset: u32) -> Option<FlashAddr> {
        let base = self.base_addr();
        if offset >= APOB_SLOT_SIZE {
            return None;
        }
        base.0.checked_add(offset).and_then(FlashAddr::new)
    }
}

pub(crate) struct ApobBufs {
    persistent_data: &'static mut [u8; APOB_PERSISTENT_DATA_STRIDE],
    page: &'static mut [u8; PAGE_SIZE_BYTES],
    scratch: &'static mut [u8; PAGE_SIZE_BYTES],
}

/// Grabs references to the static buffers.  Can only be called once.
impl ApobBufs {
    pub fn claim_statics() -> Self {
        use static_cell::ClaimOnceCell;
        static BUFS: ClaimOnceCell<(
            [u8; APOB_PERSISTENT_DATA_STRIDE],
            [u8; PAGE_SIZE_BYTES],
            [u8; PAGE_SIZE_BYTES],
        )> = ClaimOnceCell::new((
            [0; APOB_PERSISTENT_DATA_STRIDE],
            [0; PAGE_SIZE_BYTES],
            [0; PAGE_SIZE_BYTES],
        ));
        let (persistent_data, page, scratch) = BUFS.claim();
        Self {
            persistent_data,
            page,
            scratch,
        }
    }
}

/// State machine data, which implements the logic from RFD 593
///
/// See rfd.shared.oxide.computer/rfd/593#_production_strength_implementation
/// for details on the states and transitions.  Note that the diagram in the RFD
/// includes fine-grained states (e.g. writing), which the actual implementation
/// never dwells in; these states are not explicit in `ApobState`.
#[derive(Copy, Clone, PartialEq, counters::Count)]
pub(crate) enum ApobState {
    /// Waiting for `ApobStart`
    Waiting {
        #[count(children)]
        read_slot: Option<ApobSlot>,
        write_slot: ApobSlot,
    },
    /// Receiving and writing data to host flash
    Ready {
        #[count(children)]
        write_slot: ApobSlot,
        expected_length: u32,
        expected_hash: ApobHash,
        any_written: bool,
    },
    /// Writing data to flash is no longer allowed
    Locked {
        /// We store the first commit result for idempotency, because the host
        /// is allowed to retry the `ApobCommit` message.  Subsequent commits
        /// return the same result.
        commit_result: Result<(), ApobCommitError>,
    },
}

/// Persistent data, stored in Bonus Flash to select an APOB slot
#[derive(
    Copy, Clone, Eq, PartialEq, IntoBytes, FromBytes, Immutable, KnownLayout,
)]
#[repr(C)]
pub struct ApobRawPersistentData {
    /// Must always be `APOB_PERSISTENT_DATA_MAGIC`.
    oxide_magic: u32,

    /// Must always be `APOB_PERSISTENT_DATA_HEADER_VERSION` (for now)
    header_version: u32,

    /// Monotonically increasing counter
    pub monotonic_counter: u64,

    /// Either 0 or 1; directly translatable to [`ApobSlot`]
    pub slot_select: u32,

    /// CRC-32 over the rest of the data using the iSCSI polynomial
    checksum: u32,
}

impl core::cmp::PartialOrd for ApobRawPersistentData {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl core::cmp::Ord for ApobRawPersistentData {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.monotonic_counter.cmp(&other.monotonic_counter)
    }
}

impl ApobRawPersistentData {
    pub fn new(slot: ApobSlot, monotonic_counter: u64) -> Self {
        static_assertions::const_assert!(
            APOB_PERSISTENT_DATA_STRIDE
                >= core::mem::size_of::<ApobRawPersistentData>(),
        );
        let mut out = Self {
            oxide_magic: APOB_PERSISTENT_DATA_MAGIC,
            header_version: APOB_PERSISTENT_DATA_HEADER_VERSION,
            monotonic_counter,
            slot_select: match slot {
                ApobSlot::Slot0 => 0,
                ApobSlot::Slot1 => 1,
            },
            checksum: 0, // dummy value
        };
        out.checksum = out.expected_checksum();
        assert!(out.is_valid());
        out
    }

    fn expected_checksum(&self) -> u32 {
        static CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISCSI);
        let mut c = CRC.digest();
        // We do a CRC32 of everything except the checksum, which is positioned
        // at the end of the struct and is a `u32`
        let size = core::mem::size_of::<ApobRawPersistentData>()
            - core::mem::size_of::<u32>();
        c.update(&self.as_bytes()[..size]);
        c.finalize()
    }

    pub fn is_valid(&self) -> bool {
        self.oxide_magic == APOB_PERSISTENT_DATA_MAGIC
            && self.header_version == APOB_PERSISTENT_DATA_HEADER_VERSION
            && self.slot_select <= 1
            && self.checksum == self.expected_checksum()
    }
}

#[derive(Copy, Clone, PartialEq, counters::Count)]
enum Meta {
    Meta0,
    Meta1,
}

impl Meta {
    fn base_addr(&self) -> FlashAddr {
        match self {
            Meta::Meta0 => FlashAddr::new(APOB_META0_ADDR).unwrap(),
            Meta::Meta1 => FlashAddr::new(APOB_META1_ADDR).unwrap(),
        }
    }
    fn flash_addr(&self, offset: u32) -> Option<FlashAddr> {
        let base = self.base_addr();
        if offset >= APOB_META_SIZE {
            return None;
        }
        base.0.checked_add(offset).and_then(FlashAddr::new)
    }
}

impl ApobState {
    /// Initializes the `ApobState`
    ///
    /// Searches for an active slot in the metadata regions, updating the offset
    /// in the FPGA driver if found, and erases unused or invalid slots.
    pub(crate) fn init(drv: &mut FlashDriver, buf: &mut ApobBufs) -> Self {
        // Look up persistent data, which specifies an active slot
        let out = if let Some(s) = Self::get_slot(drv) {
            // Erase the inactive slot, in preparation for writing
            Self::slot_erase(drv, buf, !s);

            // Set the FPGA's offset so that the PSP reads valid data
            drv.set_apob_offset(s.base_addr());

            ApobState::Waiting {
                read_slot: Some(s),
                write_slot: !s,
            }
        } else {
            // Erase both slots
            Self::slot_erase(drv, buf, ApobSlot::Slot0);
            Self::slot_erase(drv, buf, ApobSlot::Slot1);

            // Pick a slot arbitrarily; it has just been erased and will fail
            // cryptographic checks in the PSP.
            drv.set_apob_offset(ApobSlot::Slot1.base_addr());

            ApobState::Waiting {
                read_slot: None,
                write_slot: ApobSlot::Slot0,
            }
        };
        ringbuf_entry!(Trace::State(out));
        out
    }

    fn get_raw_persistent_data(
        drv: &mut FlashDriver,
    ) -> Option<ApobRawPersistentData> {
        let a = Self::slot_scan(drv, Meta::Meta0);
        let b = Self::slot_scan(drv, Meta::Meta1);

        // None is always less than Some(..), so this picks the largest option
        a.max(b)
    }

    fn get_slot(drv: &mut FlashDriver) -> Option<ApobSlot> {
        Self::get_raw_persistent_data(drv).map(|b| match b.slot_select {
            0 => ApobSlot::Slot0,
            1 => ApobSlot::Slot1,
            // prevented by is_valid check in slot_scan
            _ => unreachable!(),
        })
    }

    /// Erases the given APOB slot
    fn slot_erase(drv: &mut FlashDriver, buf: &mut ApobBufs, slot: ApobSlot) {
        static_assertions::const_assert!(
            APOB_SLOT_SIZE.is_multiple_of(SECTOR_SIZE_BYTES)
        );
        Self::slot_erase_range(drv, buf, slot, APOB_SLOT_SIZE);
    }

    /// Erases the first `size` bytes of the given APOB slot (rounding up)
    ///
    /// `size` is rounded up to `SECTOR_SIZE_BYTES`.
    ///
    /// # Panics
    /// If `size > APOB_SLOT_SIZE`
    fn slot_erase_range(
        drv: &mut FlashDriver,
        buf: &mut ApobBufs,
        slot: ApobSlot,
        size: u32,
    ) {
        let start = userlib::sys_get_timer().now;
        ringbuf_entry!(Trace::ApobSlotErase { slot, size });
        static_assertions::const_assert!(
            (SECTOR_SIZE_BYTES as usize).is_multiple_of(PAGE_SIZE_BYTES)
        );
        let size = size.next_multiple_of(SECTOR_SIZE_BYTES);
        assert!(size <= APOB_SLOT_SIZE);

        // Read back each sector and decide whether to erase it.  We round up
        // here to the nearest sector
        let mut num_sectors_erased = 0;
        for sector_offset in (0..size).step_by(SECTOR_SIZE_BYTES as usize) {
            for page_offset in (0..SECTOR_SIZE_BYTES).step_by(PAGE_SIZE_BYTES) {
                let offset = sector_offset + page_offset;
                drv.flash_read(
                    slot.flash_addr(offset).unwrap_lite(),
                    &mut buf.page.as_mut_slice(),
                )
                .unwrap_lite();
                if buf.page.iter().any(|b| *b != 0xFF) {
                    ringbuf_entry!(Trace::ApobSlotSectorErase { slot, offset });
                    num_sectors_erased += 1;
                    drv.flash_sector_erase(
                        slot.flash_addr(offset).unwrap_lite(),
                    );
                    break;
                }
            }
        }
        let end = userlib::sys_get_timer().now;
        if num_sectors_erased > 0 {
            ringbuf_entry!(Trace::ApobSlotEraseDone {
                slot,
                time_ms: end - start,
                num_sectors_erased,
            });
        } else {
            ringbuf_entry!(Trace::ApobSlotEraseSkipped {
                slot,
                time_ms: end - start,
            });
        }
    }

    /// Finds a valid APOB slot within the given meta region
    fn slot_scan(
        drv: &mut FlashDriver,
        meta: Meta,
    ) -> Option<ApobRawPersistentData> {
        let mut best: Option<ApobRawPersistentData> = None;
        for offset in (0..APOB_META_SIZE).step_by(APOB_PERSISTENT_DATA_STRIDE) {
            let mut data = ApobRawPersistentData::new_zeroed();
            let addr = meta.flash_addr(offset).unwrap_lite();
            // flash_read is infallible when using a slice
            drv.flash_read(addr, &mut data.as_mut_bytes()).unwrap_lite();
            if data.is_valid() {
                best = best.max(Some(data));
            }
        }
        ringbuf_entry!(Trace::GotPersistentData { meta, data: best });
        best
    }

    pub(crate) fn begin(
        &mut self,
        drv: &mut FlashDriver,
        length: u32,
        algorithm: ApobHash,
    ) -> Result<(), ApobBeginError> {
        drv.check_flash_mux_state()
            .map_err(|_| ApobBeginError::InvalidState)?;
        if length > APOB_SLOT_SIZE {
            // XXX should this lock the state machine?
            return Err(ApobBeginError::BadDataLength);
        }
        match *self {
            ApobState::Waiting { write_slot, .. } => {
                *self = ApobState::Ready {
                    write_slot,
                    any_written: false,
                    expected_length: length,
                    expected_hash: algorithm,
                };
                ringbuf_entry!(Trace::State(*self));

                Ok(())
            }
            ApobState::Locked { .. } => Err(ApobBeginError::InvalidState),
            ApobState::Ready {
                expected_length,
                expected_hash,
                any_written,
                ..
            } => {
                // Idempotent begin messages are allowed
                if !any_written
                    && expected_length == length
                    && expected_hash == algorithm
                {
                    Ok(())
                } else {
                    // XXX should this lock the state machine?
                    Err(ApobBeginError::InvalidState)
                }
            }
        }
    }

    pub(crate) fn write(
        &mut self,
        drv: &mut FlashDriver,
        buf: &mut ApobBufs,
        offset: u32,
        data: Leased<R, [u8]>,
    ) -> Result<(), ApobWriteError> {
        // Check that the flash is muxed to the SP
        drv.check_flash_mux_state()
            .map_err(|_| ApobWriteError::InvalidState)?;

        // Check that the offset is within the slot
        if offset > APOB_SLOT_SIZE {
            return Err(ApobWriteError::InvalidOffset);
        }

        // Check that we're in a writable state, and set the "any written" flag
        let ApobState::Ready {
            write_slot,
            expected_length,
            any_written,
            ..
        } = self
        else {
            return Err(ApobWriteError::InvalidState);
        };
        *any_written = true;
        let write_slot = *write_slot;
        let expected_length = *expected_length;

        // Check that the end of the data range is within our expected length
        if offset
            .checked_add(data.len() as u32)
            .is_none_or(|d| d > expected_length)
        {
            return Err(ApobWriteError::InvalidSize);
        }
        for i in (0..data.len()).step_by(PAGE_SIZE_BYTES) {
            // Read data from the lease into local storage
            let n = (data.len() - i).min(PAGE_SIZE_BYTES);
            data.read_range(i..(i + n), &mut buf.page[..n])
                .map_err(|_| ApobWriteError::WriteFailed)?;
            let addr = write_slot
                .flash_addr(offset + u32::try_from(i).unwrap_lite())
                .unwrap_lite();

            // Read back the current data; it must be erased or match (for
            // idempotency)
            drv.flash_read(addr, &mut &mut buf.scratch[..n])
                .map_err(|_| ApobWriteError::WriteFailed)?;

            // This is a little tricky: we allow for bytes to either match our
            // expected write (for idempotency), _or_ to be `0xFF` (because that
            // means they're erased).  We have to check every byte to confirm
            // that they all match, but can bail immediately if we find a
            // non-matching byte that is *also* not erased.
            let mut needs_write = false;
            for (a, b) in buf.scratch[..n].iter().zip(buf.page[..n].iter()) {
                if *a != *b {
                    // You may be tempted to insert a `break` here, but that
                    // would be incorrect: there could be subsequent bytes which
                    // do not match *and* are not erased, in which case we must
                    // return `NotErased`.
                    needs_write = true;
                    if *a != 0xFF {
                        return Err(ApobWriteError::NotErased);
                    }
                }
            }
            // If any byte is not a match, then we have to do a flash write
            // (otherwise, it's an idempotent write and we can skip it)
            if needs_write {
                drv.flash_write(addr, &mut &buf.page[..n])
                    .map_err(|_| ApobWriteError::WriteFailed)?;
            }
        }
        Ok(())
    }

    pub(crate) fn read(
        &mut self,
        drv: &mut FlashDriver,
        buf: &mut ApobBufs,
        offset: u32,
        data: Leased<W, [u8]>,
    ) -> Result<usize, ApobReadError> {
        // Check that the flash is muxed to the SP
        drv.check_flash_mux_state()
            .map_err(|_| ApobReadError::InvalidState)?;

        // Check that the offset is within the slot
        if offset > APOB_SLOT_SIZE {
            return Err(ApobReadError::InvalidOffset);
        }

        // Check that we're in a writable state
        let ApobState::Waiting { read_slot, .. } = *self else {
            return Err(ApobReadError::InvalidState);
        };
        let Some(read_slot) = read_slot else {
            return Err(ApobReadError::NoValidApob);
        };

        // Check that the end of the data range is within a slot size
        if offset
            .checked_add(data.len() as u32)
            .is_none_or(|d| d > APOB_SLOT_SIZE)
        {
            return Err(ApobReadError::InvalidSize);
        }

        for i in (0..data.len()).step_by(PAGE_SIZE_BYTES) {
            // Read data from the lease into local storage
            let n = (data.len() - i).min(PAGE_SIZE_BYTES);
            let addr = read_slot.flash_addr(i as u32 + offset).unwrap_lite();

            // Read back the current data, then write it to the lease
            drv.flash_read(addr, &mut &mut buf.page[..n])
                .map_err(|_| ApobReadError::ReadFailed)?;
            data.write_range(i..(i + n), &buf.page[..n])
                .map_err(|_| ApobReadError::ReadFailed)?;
        }
        Ok(data.len())
    }

    pub(crate) fn lock(&mut self) {
        match *self {
            ApobState::Ready { .. } | ApobState::Waiting { .. } => {
                *self = ApobState::Locked {
                    commit_result: Err(ApobCommitError::InvalidState),
                };
            }
            ApobState::Locked { .. } => {
                // Nothing to do here
            }
        }
    }

    pub(crate) fn commit(
        &mut self,
        drv: &mut FlashDriver,
        buf: &mut ApobBufs,
    ) -> Result<(), ApobCommitError> {
        drv.check_flash_mux_state()
            .map_err(|_| ApobCommitError::InvalidState)?;
        let (expected_length, expected_hash, write_slot) = match *self {
            // Locking without writing anything is fine
            ApobState::Waiting { .. } => {
                *self = ApobState::Locked {
                    commit_result: Ok(()),
                };
                ringbuf_entry!(Trace::State(*self));
                return Ok(());
            }
            ApobState::Locked { commit_result } => return commit_result,
            ApobState::Ready {
                expected_length,
                expected_hash,
                write_slot,
                ..
            } => (expected_length, expected_hash, write_slot),
        };

        let r = Self::apob_validate(
            drv,
            buf,
            expected_length,
            expected_hash,
            write_slot,
        );
        *self = ApobState::Locked { commit_result: r };
        ringbuf_entry!(Trace::State(*self));

        // If validation failed, then erase the just-written data and return the
        // error code (without updating the active slot).
        if r.is_err() {
            Self::slot_erase_range(drv, buf, write_slot, expected_length);
            return r;
        }

        // We will write persistent data to flash which selects our new slot
        let old_meta_data = Self::get_raw_persistent_data(drv);
        let new_counter = old_meta_data
            .map(|p| p.monotonic_counter)
            .unwrap_or(1)
            .wrapping_add(1);
        let new_meta_data = ApobRawPersistentData::new(write_slot, new_counter);

        for m in [Meta::Meta0, Meta::Meta1] {
            Self::write_raw_persistent_data(drv, buf, new_meta_data, m);
            ringbuf_entry!(Trace::WrotePersistentData {
                data: new_meta_data,
                meta: m
            });
        }

        Ok(())
    }

    fn apob_validate(
        drv: &mut FlashDriver,
        buf: &mut ApobBufs,
        expected_length: u32,
        expected_hash: ApobHash,
        write_slot: ApobSlot,
    ) -> Result<(), ApobCommitError> {
        // Confirm that the hash of data matches our expectations
        match expected_hash {
            ApobHash::Sha256(expected_hash) => {
                let mut hasher = sha2::Sha256::new();
                use sha2::Digest;
                for i in (0..expected_length).step_by(PAGE_SIZE_BYTES) {
                    let n =
                        ((expected_length - i) as usize).min(PAGE_SIZE_BYTES);
                    let addr = write_slot.flash_addr(i).unwrap_lite();
                    drv.flash_read(addr, &mut &mut buf.page[..n])
                        .map_err(|_| ApobCommitError::CommitFailed)?;
                    hasher.update(&buf.page[..n]);
                }
                let out = hasher.finalize();
                if out != expected_hash.into() {
                    ringbuf_entry!(Trace::HashMismatch {
                        expected_hash,
                        actual_hash: out.into()
                    });
                    return Err(ApobCommitError::ValidationFailed);
                }
            }
        }

        // Check the APOB itself
        let mut header = apob::ApobHeader::new_zeroed();
        let addr = write_slot.flash_addr(0).unwrap_lite();
        drv.flash_read(addr, &mut header.as_mut_bytes())
            .unwrap_lite();
        if header.sig != apob::APOB_SIG {
            ringbuf_entry!(Trace::BadApobSig {
                expected: apob::APOB_SIG,
                actual: header.sig
            });
            return Err(ApobCommitError::ValidationFailed);
        }
        if header.version != apob::APOB_VERSION {
            ringbuf_entry!(Trace::BadApobVersion {
                expected: apob::APOB_VERSION,
                actual: header.version,
            });
            return Err(ApobCommitError::ValidationFailed);
        }
        if header.size != expected_length {
            ringbuf_entry!(Trace::BadApobSize {
                expected: expected_length,
                actual: header.size,
            });
            return Err(ApobCommitError::ValidationFailed);
        }
        let mut pos = header.offset;
        while pos < expected_length {
            let mut entry = apob::ApobEntry::new_zeroed();
            let addr = write_slot.flash_addr(pos).unwrap_lite();
            drv.flash_read(addr, &mut entry.as_mut_bytes())
                .unwrap_lite();
            pos += entry.size;
        }
        if pos != expected_length {
            ringbuf_entry!(Trace::BadApobWalk {
                expected: expected_length,
                actual: pos,
            });
            return Err(ApobCommitError::ValidationFailed);
        }

        Ok(())
    }

    fn write_raw_persistent_data(
        drv: &mut FlashDriver,
        buf: &mut ApobBufs,
        data: ApobRawPersistentData,
        meta: Meta,
    ) {
        let mut found: Option<FlashAddr> = None;
        for offset in (0..APOB_META_SIZE).step_by(APOB_PERSISTENT_DATA_STRIDE) {
            let addr = meta.flash_addr(offset).unwrap_lite();
            // Infallible when using a slice
            drv.flash_read(addr, &mut buf.persistent_data.as_mut_slice())
                .unwrap_lite();
            if buf.persistent_data.iter().all(|c| *c == 0xFF) {
                found = Some(addr);
                break;
            }
        }
        let addr = found.unwrap_or_else(|| {
            let addr = meta.flash_addr(0).unwrap_lite();
            drv.flash_sector_erase(addr);
            addr
        });
        // Infallible when using a slice
        drv.flash_write(addr, &mut data.as_bytes()).unwrap_lite();
    }
}
