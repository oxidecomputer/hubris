// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![feature(naked_functions)]

//! Hypovisor calls

pub use lpc55_flash::{
    HypoStatus, UpdateTarget, __write_block, __erase_block, FLASH_PAGE_SIZE,
};

pub const TABLE_MAGIC: u32 = 0xabcd_abcd;

#[macro_export]
macro_rules! declare_tz_table {
    () => {
        #[no_mangle]
        #[used]
        #[link_section = ".tz_table"]
        static TZ_TABLE: SecureTable = SecureTable {
            magic: 0,
            write_to_flash: None,
            erase_flash: None,
        };
    };
}

#[macro_export]
macro_rules! declare_not_tz_table {
    () => {
        #[no_mangle]
        #[used]
        #[link_section = ".tz_table"]
        static TZ_TABLE: SecureTable = SecureTable {
            magic: TABLE_MAGIC,
            write_to_flash: Some(__write_block),
            erase_flash: Some(__erase_block),
        };
    };
}

#[macro_export]
macro_rules! tz_table {
    () => {
        &TZ_TABLE
    };
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct SecureTable {
    pub magic: u32,
    // This is modeled as an option to avoid the need for a useless stub
    // function
    pub write_to_flash:
        Option<unsafe extern "C" fn(UpdateTarget, u32, *mut u8) -> HypoStatus>,
    pub erase_flash:
        Option<unsafe extern "C" fn(UpdateTarget, u32) -> HypoStatus>,
}

impl SecureTable {
    pub unsafe fn write_to_flash(
        &self,
        img: UpdateTarget,
        block_num: u32,
        buf: *mut u8,
    ) -> HypoStatus {
        // SAFETY
        // This entire function gets marked as unsafe because it takes a
        // raw pointer (necessary for TrustZone ABI reasons)
        //
        // The table of applicable secure function calls is updated
        // at build time. The compiler doesn't know this and wants to
        // optimize this function based on the initial state which isn't
        // what we want.
        //
        // We're doing a volatile read of the magic and comparing it to
        // what we expect. If it is what we expect, we can assume the
        // function table is what we generated at build time.
        let magic = core::ptr::read_volatile(&self.magic);
        if magic != TABLE_MAGIC {
            panic!();
        }
        if let Some(func) = core::ptr::read_volatile(&self.write_to_flash) {
            return func(img, block_num, buf);
        }
        unreachable!()
    }

    pub unsafe fn erase_flash(
        &self,
        img: UpdateTarget,
        block_num: u32,
    ) -> HypoStatus {
        // SAFETY
        // This entire function gets marked as unsafe because it takes a
        // raw pointer (necessary for TrustZone ABI reasons)
        //
        // The table of applicable secure function calls is updated
        // at build time. The compiler doesn't know this and wants to
        // optimize this function based on the initial state which isn't
        // what we want.
        //
        // We're doing a volatile read of the magic and comparing it to
        // what we expect. If it is what we expect, we can assume the
        // function table is what we generated at build time.
        let magic = core::ptr::read_volatile(&self.magic);
        if magic != TABLE_MAGIC {
            panic!();
        }
        if let Some(func) = core::ptr::read_volatile(&self.erase_flash) {
            return func(img, block_num);
        }
        unreachable!()
    }
}
