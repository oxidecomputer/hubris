// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use hubpack::SerializedSize;
use lpc55_pac::syscon::RegisterBlock;
use serde::Serialize;
use stage0_handoff::{HandoffData, HandoffDataHeader};

/// The Handoff type is a thin wrapper over the memory region used to transfer
/// image boot state and DICE artifacts (seeds & certs) from stage0 to hubris
/// tasks. It is intended for use by stage0 to write these artifacts to memory
/// where they will later be read out by a hubris task.
pub struct Handoff<'a>(&'a RegisterBlock);

impl<'a> Handoff<'a> {
    // Handing off artifacts through the USB SRAM requires we power it on.
    // We implement this as a constructor on the producer side of the handoff
    // to ensure this memory is enabled before consumers attempt access.
    // Attempts to access this memory region before powering it on will fault.
    pub fn turn_on(syscon: &'a RegisterBlock) -> Self {
        syscon.ahbclkctrl2.modify(|_, w| w.usb1_ram().enable());
        syscon
            .presetctrl2
            .modify(|_, w| w.usb1_ram_rst().released());

        Self(syscon)
    }

    pub fn turn_off(self) {
        self.0
            .presetctrl2
            .modify(|_, w| w.usb1_ram_rst().asserted());
        self.0.ahbclkctrl2.modify(|_, w| w.usb1_ram().disable());
    }

    pub fn store<T>(&self, t: &T) -> usize
    where
        T: HandoffData + SerializedSize + Serialize,
    {
        // Cast MEM_RANGE from HandoffData to a mutable slice.
        //
        // SAFETY: This unsafe block relies on implementers of the HandoffData
        // trait to validate the memory range denoted by Self::MEM_RANGE. Each
        // implementation in this module is checked by static assertion.
        let dst = unsafe {
            core::slice::from_raw_parts_mut(
                T::MEM_RANGE.start as *mut u8,
                T::MAX_SIZE + HandoffDataHeader::MAX_SIZE,
            )
        };

        // Serialize the header for the handoff data
        let n = hubpack::serialize(dst, &T::header())
            .expect("handoff store header");

        // Serialize the data
        n + hubpack::serialize(&mut dst[n..], t).expect("handoff store value")
    }
}
