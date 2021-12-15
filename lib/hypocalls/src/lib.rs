// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![feature(asm)]
#![feature(naked_functions)]

pub use lpc55_romapi::FlashStatus;

/// Write the buffer to the specified region number.
///
/// Once we've established our regions this should be changed to an enum
/// or something else representative
#[cfg(not(feature = "standalone"))]
#[inline(never)]
pub fn hypo_write_to_flash(region: u32, buf: &[u8]) -> FlashStatus {
    use num_traits::cast::FromPrimitive;

    let result = unsafe {
        core::mem::transmute::<
            _,
            unsafe extern "C" fn(u32, *const u8, u32) -> u32,
        >(__bootloader_fn_table.write_to_flash)(
            region,
            buf.as_ptr(),
            buf.len() as u32,
        )
    };

    let result = match FlashStatus::from_u32(result) {
        Some(a) => a,
        None => FlashStatus::Unknown,
    };

    return result;
}

#[cfg(feature = "standalone")]
pub fn hypo_write_to_flash(_addr: u32, _buf: &[u8], _size: u32) -> FlashStatus {
    return FlashStatus::Success;
}
include!(concat!(env!("OUT_DIR"), "/hypo.rs"));
