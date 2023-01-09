// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
#![feature(naked_functions)]
#![no_std]
#![no_main]

use hypocalls::*;
#[allow(unused_imports)]
use userlib::*;

#[link_section = ".tz_table"]
#[no_mangle]
#[used]
static TZ_TABLE: SecureTable = SecureTable {
    magic: TABLE_MAGIC,
    write_to_flash: Some(write_to_flash),
    erase_flash: Some(erase_flash),
};

#[export_name = "main"]
fn main() -> ! {
    // This is a special task we never expect to actually start as a normal
    // hubris task. See README for more details.
    cortex_m::asm::udf();
}

#[naked]
#[no_mangle]
#[link_section = ".nsc"]
pub unsafe extern "C" fn write_to_flash(
    image_num: UpdateTarget,
    page_num: u32,
    buffer: *mut u8,
) -> HypoStatus {
    // ARM really wants another function to branch to based on
    // their secure docs. We also don't have full compiler support yet
    // so for now just keep it simple and have a single function with
    // the sg instruction
    //
    // The sg is a nop when not using TrustZone. This will need to be
    // a bxns when we get full TrustZone support
    core::arch::asm!(
        "
        sg
        push {{lr}}
        bl __write_block
        pop {{lr}}
        bxns lr
        ",
        options(noreturn)
    );
}

#[naked]
#[no_mangle]
#[link_section = ".nsc"]
pub unsafe extern "C" fn erase_flash(
    image_num: UpdateTarget,
    page_num: u32,
) -> HypoStatus {
    // ARM really wants another function to branch to based on
    // their secure docs. We also don't have full compiler support yet
    // so for now just keep it simple and have a single function with
    // the sg instruction
    //
    // The sg is a nop when not using TrustZone. This will need to be
    // a bxns when we get full TrustZone support
    core::arch::asm!(
        "
        sg
        push {{lr}}
        bl __erase_block
        pop {{lr}}
        bxns lr
        ",
        options(noreturn)
    );
}
