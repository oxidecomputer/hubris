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
    ApobBeginError, ApobHash, ApobReadError, ApobWriteError, HfError,
};
use idol_runtime::{Leased, R, W};
use ringbuf::{ringbuf, ringbuf_entry};
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
pub const APOB_PERSISTENT_DATA_STRIDE: u32 = 128;
pub const APOB_PERSISTENT_DATA_HEADER_VERSION: u32 = 1;

pub const APOB_META_SIZE: u32 = SECTOR_SIZE_BYTES;
pub const APOB_SLOT_SIZE: u32 = 2 * 1024 * 1024; // 2 MiB (chosen arbitrarily)

// The layout is [meta0, meta1, slot0, slot1]
pub const APOB_META0_ADDR: u32 = crate::hf::SLOT_SIZE_BYTES * 2;
pub const APOB_META1_ADDR: u32 = APOB_META0_ADDR + APOB_META_SIZE;
pub const APOB_SLOT0_ADDR: u32 = APOB_META1_ADDR + APOB_META_SIZE;
pub const APOB_SLOT1_ADDR: u32 = APOB_SLOT0_ADDR + APOB_SLOT_SIZE;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    ApobSlotErase { slot: ApobSlot },
    ApobSlotEraseSkip { slot: ApobSlot },
}
ringbuf!(Trace, 16, Trace::None);

#[derive(Copy, Clone, PartialEq)]
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

#[derive(Copy, Clone)]
pub(crate) enum ApobState {
    /// Waiting for `ApobStart`
    Waiting {
        apob_write_slot: ApobSlot,
        apob_read_slot: Option<ApobSlot>,
    },
    /// Receiving and writing data to host flash
    Ready {
        apob_write_slot: ApobSlot,
        expected_length: u64,
        expected_hash: ApobHash,
    },
    /// We have finished writing data to flash
    Locked,
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
            APOB_PERSISTENT_DATA_STRIDE as usize
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
    pub(crate) fn init(drv: &mut FlashDriver) -> Self {
        if let Some(s) = Self::get_apob_slot(drv) {
            drv.set_apob_offset(s.base_addr());
            ApobState::Waiting {
                apob_read_slot: Some(s),
                apob_write_slot: !s,
            }
        } else {
            ApobState::Waiting {
                apob_read_slot: None,
                apob_write_slot: ApobSlot::Slot0,
            }
        }
    }

    /// Finds the currently active APOB slot, erasing any unused slots
    fn get_apob_slot(drv: &mut FlashDriver) -> Option<ApobSlot> {
        let a = Self::apob_slot_scan(drv, Meta::Meta0);
        let b = Self::apob_slot_scan(drv, Meta::Meta1);

        let best = match (a, b) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(_), None) => a,
            (None, Some(_)) => b,
            (None, None) => None,
        };

        // Erase inactive slot
        match best.map(|b| b.slot_select) {
            Some(0) => {
                Self::apob_slot_erase(drv, ApobSlot::Slot1);
                Some(ApobSlot::Slot0)
            }
            Some(1) => {
                Self::apob_slot_erase(drv, ApobSlot::Slot0);
                Some(ApobSlot::Slot0)
            }
            Some(_) => {
                unreachable!(); // prevented by is_valid check
            }
            None => {
                Self::apob_slot_erase(drv, ApobSlot::Slot0);
                Self::apob_slot_erase(drv, ApobSlot::Slot1);
                None
            }
        }
    }

    /// Erases the given APOB slot
    fn apob_slot_erase(drv: &mut FlashDriver, slot: ApobSlot) {
        let mut dirty = false;
        let mut buf = [0u8; PAGE_SIZE_BYTES];
        for offset in 0..APOB_SLOT_SIZE / PAGE_SIZE_BYTES as u32 {
            drv.flash_read(
                slot.flash_addr(offset * PAGE_SIZE_BYTES as u32)
                    .unwrap_lite(),
                &mut buf.as_mut_slice(),
            )
            .unwrap_lite();
            if buf.iter().any(|b| *b != 0xFF) {
                dirty = true;
                break;
            }
        }

        if dirty {
            ringbuf_entry!(Trace::ApobSlotErase { slot });
            for offset in 0..APOB_SLOT_SIZE / SECTOR_SIZE_BYTES {
                drv.flash_sector_erase(
                    slot.flash_addr(offset * SECTOR_SIZE_BYTES).unwrap_lite(),
                )
            }
        } else {
            ringbuf_entry!(Trace::ApobSlotEraseSkip { slot });
        }
    }

    /// Finds a valid APOB slot within the given meta region
    fn apob_slot_scan(
        drv: &mut FlashDriver,
        meta: Meta,
    ) -> Option<ApobRawPersistentData> {
        let mut best: Option<ApobRawPersistentData> = None;
        for i in 0..APOB_META_SIZE / APOB_PERSISTENT_DATA_STRIDE {
            let mut data = ApobRawPersistentData::new_zeroed();
            let offset = i * APOB_PERSISTENT_DATA_STRIDE;
            let addr = meta.flash_addr(offset).unwrap_lite();
            drv.flash_read(addr, &mut data.as_mut_bytes()).unwrap_lite(); // infallible when using a slice
            best = best.max(Some(data).filter(|d| d.is_valid()));
        }
        best
    }

    pub(crate) fn apob_begin(
        &mut self,
        drv: &mut FlashDriver,
        length: u64,
        algorithm: ApobHash,
    ) -> Result<(), ApobBeginError> {
        drv.check_flash_mux_state()
            .map_err(|_| ApobBeginError::InvalidState)?;
        if length > u64::from(APOB_SLOT_SIZE) {
            return Err(ApobBeginError::BadDataLength);
        }
        match *self {
            ApobState::Waiting {
                apob_write_slot, ..
            } => {
                *self = ApobState::Ready {
                    apob_write_slot,
                    expected_length: length,
                    expected_hash: algorithm,
                };
                Ok(())
            }
            ApobState::Locked => Err(ApobBeginError::InvalidState),
            ApobState::Ready {
                expected_length,
                expected_hash,
                ..
            } => {
                // Allow idempotent Begin messages
                if expected_length == length && expected_hash == algorithm {
                    Ok(())
                } else {
                    // XXX should this lock the state machine?
                    Err(ApobBeginError::InvalidState)
                }
            }
        }
    }

    pub(crate) fn apob_write(
        &mut self,
        drv: &mut FlashDriver,
        offset: u64,
        data: Leased<R, [u8]>,
    ) -> Result<(), ApobWriteError> {
        // Check that the flash is muxed to the SP
        drv.check_flash_mux_state()
            .map_err(|_| ApobWriteError::InvalidState)?;

        // Check that the offset is within the slot
        if offset > u64::from(APOB_SLOT_SIZE) {
            return Err(ApobWriteError::InvalidOffset);
        }

        // Check that we're in a writable state
        let ApobState::Ready {
            apob_write_slot,
            expected_length,
            ..
        } = *self
        else {
            return Err(ApobWriteError::InvalidState);
        };

        // Check that the end of the data range is within our expected length
        if offset
            .checked_add(data.len() as u64)
            .is_none_or(|d| d > expected_length)
        {
            return Err(ApobWriteError::InvalidSize);
        }

        let mut out_buf = [0u8; PAGE_SIZE_BYTES];
        let mut scratch_buf = [0u8; PAGE_SIZE_BYTES];
        for i in (0..data.len()).step_by(PAGE_SIZE_BYTES) {
            // Read data from the lease into local storage
            let n = (data.len() - i).min(PAGE_SIZE_BYTES);
            data.read_range(i..(i + n), &mut out_buf[..n])
                .map_err(|_| ApobWriteError::WriteFailed)?;
            let addr = apob_write_slot
                .flash_addr(i.try_into().unwrap_lite())
                .unwrap_lite();

            // Read back the current data; it must be erased or match (for
            // idempotency)
            drv.flash_read(addr, &mut &mut scratch_buf[..n])
                .map_err(|_| ApobWriteError::WriteFailed)?;
            if scratch_buf[..n]
                .iter()
                .zip(out_buf[..n].iter())
                .any(|(a, b)| *a != *b && *a != 0xFF)
            {
                return Err(ApobWriteError::NotErased);
            }
            drv.flash_write(addr, &mut &out_buf[..n])
                .map_err(|_| ApobWriteError::WriteFailed)?;
        }
        Ok(())
    }

    pub(crate) fn apob_read(
        &mut self,
        drv: &mut FlashDriver,
        offset: u64,
        data: Leased<W, [u8]>,
    ) -> Result<usize, ApobReadError> {
        // Check that the flash is muxed to the SP
        drv.check_flash_mux_state()
            .map_err(|_| ApobReadError::InvalidState)?;

        // Check that the offset is within the slot
        if offset > u64::from(APOB_SLOT_SIZE) {
            return Err(ApobReadError::InvalidOffset);
        }

        // Check that we're in a writable state
        let ApobState::Waiting { apob_read_slot, .. } = *self else {
            return Err(ApobReadError::InvalidState);
        };
        let Some(apob_read_slot) = apob_read_slot else {
            // XXX dedicated error type here?
            return Err(ApobReadError::InvalidState);
        };

        // Check that the end of the data range is within a slot size
        if offset
            .checked_add(data.len() as u64)
            .is_none_or(|d| d > u64::from(APOB_SLOT_SIZE))
        {
            return Err(ApobReadError::InvalidSize);
        }

        let mut out_buf = [0u8; PAGE_SIZE_BYTES];
        for i in (0..data.len()).step_by(PAGE_SIZE_BYTES) {
            // Read data from the lease into local storage
            let n = (data.len() - i).min(PAGE_SIZE_BYTES);
            let addr = apob_read_slot
                .flash_addr(i.try_into().unwrap_lite())
                .unwrap_lite();

            // Read back the current data; it must be erased or match (for
            // idempotency)
            drv.flash_read(addr, &mut &mut out_buf[..n])
                .map_err(|_| ApobReadError::ReadFailed)?;

            data.write_range(i..(i + n), &out_buf[..n])
                .map_err(|_| ApobReadError::ReadFailed)?;
        }
        Ok(data.len() as usize)
    }
}
