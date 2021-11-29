// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hypovisor calls

use lpc55_romapi::FlashStatus;

// FlashStatus is represented as a u32 so it's safe to return directly.
// We convert on the receiving end for safety
// #[cmse_nonsecure_entry] We want this eventually
#[no_mangle]
pub unsafe extern "C" fn __write_to_flash(
    which: u32,
    buffer: *mut u32,
    len: u32,
) -> FlashStatus {
    extern "C" {
        static address_of_test_region: u32;
    }

    if len == 0 {
        return FlashStatus::InvalidArg;
    }

    // We expect this to be called from non-secure (running on 28) and
    // non-privileged mode (called from hubris task). The tt instructions
    // are mostly useless for doing any kind of checking on the buffer
    // address passed in. The failure mode is going to be a fault.

    if which == 0 {
        let flash_addr = address_of_test_region as *const u32 as u32;

        if let Err(result) = lpc55_romapi::flash_erase(flash_addr, len) {
            return result;
        }

        if let Err(result) = lpc55_romapi::flash_write(flash_addr, buffer, len)
        {
            return result;
        }

        return FlashStatus::Success;
    }

    return FlashStatus::InvalidArg;
}

#[link_section = ".flash_hypo"]
#[naked]
#[no_mangle]
pub unsafe extern "C" fn write_to_flash(
    _addr: u32,
    _buffer: *mut u32,
    _len: u32,
) -> u32 {
    // ARM really wants another function to branch to based on
    // their secure docs. We also don't have full compiler support yet
    // so for now just keep it simple and have a single function with
    // the sg instruction
    //
    // The sg is a nop when not using TrustZone. This will need to be
    // a bxns when we get full TrustZone support
    asm!(
        "
        sg
        push {{lr}}
        bl __write_to_flash
        pop {{lr}}
        bx lr
        ",
        options(noreturn)
    );
}
