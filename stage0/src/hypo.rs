//! Hypovisor calls

use lpc55_romapi::FlashStatus;

// FlashStatus is represented as a u32 so it's safe to return directly.
// We convert on the receiving end for safety
#[cmse_nonsecure_entry]
#[no_mangle]
pub unsafe extern "C" fn __write_to_flash(
    which: u32,
    buffer: *mut u32,
    len: u32,
) -> FlashStatus {
    if which == 0 {
        if let Err(result) = lpc55_romapi::flash_erase(0x90000, 0x8000) {
            return result;
        }

        if let Err(result) = lpc55_romapi::flash_write(0x90000, buffer, len) {
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
    asm!(
        "
        sg
        push {{lr}}
        bl __write_to_flash
        pop {{lr}}
        bxns lr
        ",
        options(noreturn)
    );
}
