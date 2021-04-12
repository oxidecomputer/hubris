//! Hypovisor calls

use bitfield::bitfield;
use lpc55_romapi::FlashStatus;

// TODO Convert over to the cortex-m version
bitfield! {
    /// Test Target Response Payload
    ///
    /// Provides the response payload from a TT, TTA, TTT or TTAT instruction.
    #[derive(PartialEq, Copy, Clone)]
    struct TtResp(u32);
    impl Debug;
    mregion, _: 7, 0;
    sregion, _: 15, 8;
    mrvalid, _: 16;
    srvalid, _: 17;
    r, _: 18;
    rw, _: 19;
    nsr, _: 20;
    nsrw, _: 21;
    s, _: 22;
    irvalid, _: 23;
    iregion, _: 31, 24;
}

// FlashStatus is represented as a u32 so it's safe to return directly.
// We convert on the receiving end for safety
#[cmse_nonsecure_entry]
#[no_mangle]
pub unsafe extern "C" fn __write_to_flash(
    which: u32,
    buffer: *mut u32,
    len: u32,
) -> FlashStatus {

    if len == 0 {
        return FlashStatus::InvalidArg;
    }

    // Per C2.4.247 of the ARMv8m Manal
    //
    // Test Target Alternate Domain (TTA) and Test Target Alternate Domain
    // Unprivileged (TTAT) query the Securitystate and access permissions of a
    // memory location for a Non-secure access to that location. These
    // instructions areonly valid when executing in Secure state, and are
    // UNDEFINED if used from Non-secure state.
    //
    let mut start_result: u32;
    let mut end_result: u32;

    let end_addr = match (buffer as u32).checked_add(len - 1) {
        Some(s) => s,
        None => return FlashStatus::InvalidArg,
    };

    asm!("
        ttat {result}, {addr}
        ",
        addr = in(reg) buffer,
        result = out(reg) start_result);

    asm!("
        ttat {result}, {addr}
        ",
        addr = in(reg) end_addr,
        result = out(reg) end_result);

    // If start and end have different access bits something has gone wrong
    if start_result != end_result {
        return FlashStatus::InvalidArg;
    }

    let resp = TtResp(start_result);

    // Secure buffers are not allowed to be written via this API
    if resp.s() {
        return FlashStatus::InvalidArg;
    }

    // We need to be able to read
    if !(resp.r() && resp.nsr()) {
        return FlashStatus::InvalidArg;
    }

    // TODO Check against SAU regions as well. Right now those are hard coded.

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
