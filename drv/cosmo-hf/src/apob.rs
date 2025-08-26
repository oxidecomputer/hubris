// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tools to extract the APOB location from an AMD ROM
//!
//! For details, see AMD document 57299; tables and sections in this code refer
//! to Rev. 2.0 February 2025.

use crate::hf::ServerImpl;
use drv_hf_api::HfError;
use userlib::UnwrapLite;
use zerocopy::{FromBytes, Immutable, IntoBytes};

/// Embedded firmware structure (Table 3)
///
/// Only relevant fields are included here.
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
    _unused1: u8,
    _unused2: u8,
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

    /// Find the APOB location from the bonus flash region
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
