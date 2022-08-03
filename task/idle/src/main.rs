// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
extern crate userlib;

#[export_name = "main"]
fn main() -> ! {
    loop {
        if cfg!(feature = "insomniac") {
            // In insomniac-mode, we just spinloop to absorb idle cycles. This
            // is useful on certain processors where entering a low-power state
            // interrupts debugging.
            //
            // Note that this is an explicit nop rather than an empty block
            // because an empty `loop {}` is technically UB and will be replaced
            // by a trap, bringing the system to a halt with no tasks runnable.
            // So, do not get clever and remove this.
            cortex_m::asm::nop();
        } else {
            // Wait For Interrupt to pause the processor until an ISR arrives,
            // which could wake some higher-priority task.
            cortex_m::asm::wfi();
        }
    }
}
