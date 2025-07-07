// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//! The measurement handoff is a token telling the SP that it has been measured
//!
//! For various reasons (see RFD 568), the RoT is not allowed to proactively
//! reset the SP; it can only catch the SP during a reset and hold it for
//! measurements.  However, during initial power-on, the SP boots faster than
//! the RoT.  What are we to do?
//!
//! RFD 568 proposes a coordination mechanism: the SP will reset itself a few
//! times, until either a retry count is exceeded or it boots with a token
//! deposited in a particular memory location (indicating that it has been
//! measured).
//!
//! We store 4 `u64` words at the beginning of a "handoff" region, which is
//! expected to be DTCM (`0x2000_0000`). The words are as follows:
//!
//! - Measurement token, which is `MEASUREMENT TOKEN` if the SP has been
//!   measured, or any other value if not.
//! - Counter token, which is `COUNTER_TAG` if the subsequent word is expected
//!   to be a counter value.
//! - Counter value indicating the number of resets; this starts at 1 and counts
//!   up from there.
//! - Counter check word, which is `COUNTER_TAG` xor'd with the counter value.
//!   If the counter check word is incorrect, then the counter is reset to 0.
#![no_std]

pub enum MeasurementResult {
    Measured,
    NotMeasured(u64),
}

pub const MEASUREMENT_TOKEN: u64 = 0xc887a12b17ed35f7;
pub const MEASUREMENT_BASE: usize = 0x2000_0000;
const COUNTER_TAG: u64 = 0x4e423d17176f5b51;

extern "C" {
    static mut _HANDOFF_REGION_BASE: [u8; 0];
    static mut _HANDOFF_REGION_END: [u8; 0];
}

/// Check the measurement token, calling `reset_fn` to reset if needed
///
/// Calls `reset_fn` (which diverges) if no measurement is present and we have
/// not yet exceeded our retry count; otherwise, returns `true` if the
/// measurement is valid, or `false` if we exceeded `retry_count`.
///
/// `reset_fn` should include a delay, to give the RoT time to boot.
pub unsafe fn check(retry_count: u64, reset_fn: fn() -> !) -> bool {
    let ptr: *mut u64 = &raw mut _HANDOFF_REGION_BASE as *mut _;
    let end: *mut u64 = &raw mut _HANDOFF_REGION_END as *mut _;
    assert!(ptr == MEASUREMENT_BASE as *mut _);
    assert!(end.offset_from(ptr) >= 4 * core::mem::size_of::<u64>() as isize);

    match check_measurement(ptr) {
        MeasurementResult::Measured => true,
        MeasurementResult::NotMeasured(i) => {
            if i < retry_count {
                reset_fn();
            } else {
                clear(ptr); // too many retries, exit
                false
            }
        }
    }
}

unsafe fn check_measurement(ptr: *mut u64) -> MeasurementResult {
    let token = core::ptr::read_volatile(ptr);
    let tag = core::ptr::read_volatile(ptr.wrapping_add(1));
    let mut counter = core::ptr::read_volatile(ptr.wrapping_add(2));
    let check = core::ptr::read_volatile(ptr.wrapping_add(3));

    if token == MEASUREMENT_TOKEN {
        clear(ptr);
        MeasurementResult::Measured
    } else if tag != COUNTER_TAG || tag ^ counter != check {
        write_counter(ptr, 1);
        MeasurementResult::NotMeasured(0)
    } else {
        counter += 1;
        write_counter(ptr, counter);
        MeasurementResult::NotMeasured(counter)
    }
}

unsafe fn clear(ptr: *mut u64) {
    core::ptr::write_volatile(ptr, 0);
    core::ptr::write_volatile(ptr.wrapping_add(1), 0);
}

unsafe fn write_counter(ptr: *mut u64, counter: u64) {
    core::ptr::write_volatile(ptr.wrapping_add(1), COUNTER_TAG);
    core::ptr::write_volatile(ptr.wrapping_add(2), counter);
    core::ptr::write_volatile(ptr.wrapping_add(3), counter ^ COUNTER_TAG);
}
