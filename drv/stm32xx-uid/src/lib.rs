// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A simple driver to read the STM32's UID.  This crate must be used with
//! either the `family-stm32g0` or `family-stm32h7` feature enabled, and
//! must be called from a task which has access to the system flash region.

#![no_std]

// The UID address is in System (flash) Memory, rather than in a peripheral,
// so it's not documented in the SVD or `stm32` crate.
cfg_if::cfg_if! {
    if #[cfg(feature = "family-stm32g0")] {
        const UID_ADDR: u32 = 0x1FFF_7590;
    } else if #[cfg(feature = "family-stm32h7")] {
        const UID_ADDR: u32 = 0x1FF1_E800;
    } else if #[cfg(feature = "family-stm32f4")] {
        const UID_ADDR: u32 = 0x1FFF_7A10;
    } else {
        compile_error!("unsupported SoC family");
        const UID_ADDR: u32 = 0; // Prevents a second error below
    }
}

/// Read the 96-bit UID
pub fn read_uid() -> [u32; 3] {
    let uid = unsafe { core::slice::from_raw_parts(UID_ADDR as *const u32, 3) };
    [uid[0], uid[1], uid[2]]
}
