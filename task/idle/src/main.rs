// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
extern crate userlib;

#[unsafe(export_name = "main")]
fn main() -> ! {
    loop {
        // In insomniac-mode, we just spinloop to absorb idle cycles. This
        // is useful on certain processors where entering a low-power state
        // interrupts debugging.
        //
        // An empty loop *used* to cause UB, due to a bug in LLVM (see
        // https://github.com/rust-lang/rust/issues/28728), but was later fixed
        // by the upgrade to LLVM12, which resolved this in Rust 1.52.0 (see
        // https://github.com/rust-lang/rust/pull/81451).
        //
        // Wait For Interrupt to pause the processor until an ISR arrives,
        // which could wake some higher-priority task.
        #[cfg(not(feature = "insomniac"))]
        cortex_m::asm::wfi();
    }
}
