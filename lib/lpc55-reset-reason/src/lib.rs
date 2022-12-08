// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

//use num_derive::FromPrimitive;
//use num_traits::FromPrimitive;
use lpc55_pac as device;

#[repr(u32)]
#[derive(Debug)]
pub enum Lpc55ResetReason {
    PowerOn,
    Pin,
    BrownOut,
    // System reset (i.e. reset via AIRCR)
    System,
    Watchdog,
    //Other,
    Other(u32),
}

// See section 13.4.13 of v2.4 of UM11126
const POR: u32 = 1 << 4;
const PADRESET: u32 = 1 << 5;
const BODRESET: u32 = 1 << 6;
const SYSTEMRESET: u32 = 1 << 7;
const WDTRESET: u32 = 1 << 8;

pub fn get_reset_reason() -> Lpc55ResetReason {
    let pmc = unsafe { &*device::PMC::ptr() };

    // The Reset Reason is stored in the AOREG1 register in the power
    // management block. This crypticly named register is set based
    // on another undocumented register in the power management space.
    let aoreg1 = pmc.aoreg1.read().bits();

    match aoreg1 {
        POR => Lpc55ResetReason::PowerOn,
        PADRESET => Lpc55ResetReason::Pin,
        BODRESET => Lpc55ResetReason::BrownOut,
        SYSTEMRESET => Lpc55ResetReason::System,
        WDTRESET => Lpc55ResetReason::Watchdog,
        _ => Lpc55ResetReason::Other(aoreg1),
    }
}
