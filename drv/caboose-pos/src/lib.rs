// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Position of the caboose within this image
//!
//! This is patched in by the build system, in the same way that we patch
//! values in the `task_slot!` macro.

#![no_std]

use unwrap_lite::UnwrapLite;
use volatile_const::VolatileConst;

#[repr(C)]
pub struct CaboosePos(VolatileConst<[u32; 2]>);

impl CaboosePos {
    /// A `CaboosePos` that has not been resolved by a later processing step.
    ///
    /// Calling `as_slice()` on an unbound `CaboosePos` will return `None`
    pub const UNBOUND: Self = Self(VolatileConst::new([0, 0]));

    pub fn as_slice(&self) -> Option<&'static [u8]> {
        let [start, end] = self.0.get();
        if start == 0 && end == 0 {
            None
        } else {
            // SAFETY: these values are given to us by the build system, and
            // should point to a region in flash memory that does not exceed the
            // bounds of flash.
            unsafe {
                Some(core::slice::from_raw_parts(
                    start as *const u8,
                    end.checked_sub(start)
                        .and_then(|i| i.try_into().ok())
                        .unwrap_lite(),
                ))
            }
        }
    }
}

#[used]
pub static CABOOSE_POS: CaboosePos = CaboosePos::UNBOUND;

#[repr(C)]
struct CaboosePosTableEntry(*const [u32; 2]);

// This is used as a message to the build system
#[used]
#[link_section = ".caboose_pos_table"]
static _CABOOSE_POS_TABLE_ENTRY: CaboosePosTableEntry =
    CaboosePosTableEntry(CABOOSE_POS.0.as_ptr());

// SAFETY
//
// Storing a pointer in a struct causes it to not implement Sync automatically.
// In this case, CaboosePosTableEntry is only ever constructed right here in
// this file, and points to a static variable.  Thus, the stored pointer can
// only be to a static CaboosePos.  Further, we place the CaboosePosTableEntry
// in a .caboose_pos_table linker section that is treated similar to debug
// information in that no virtual addresses are allocated to the contents and
// the section is not loaded into the process space.  As such, instances of
// CaboosePosTableEntry will never exist at runtime.
unsafe impl Sync for CaboosePosTableEntry {}
