// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal driver to read the IDC register in the DBGMCU

#![no_std]

#[cfg(feature = "h743")]
pub use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
pub use stm32h7::stm32h753 as device;

pub fn read_idc() -> u32 {
    // SAFETY: this is either allowed by the MPU configuration or will crash
    // (which is safe)
    let dbg = unsafe { &*device::DBGMCU::ptr() };
    dbg.idc.read().bits()
}
