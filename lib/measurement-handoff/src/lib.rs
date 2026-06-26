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
//! We store 4 `u32` words at the beginning of a "handoff" region, which is
//! expected to be DTCM (`0x2000_0000`). The words are as follows:
//!
//! - Measurement token, which is `measurement_token::VALID` (written by the RoT)
//!   if the SP has been measured, `measurement_token::SKIP` (written by a
//!   debugger) if an external debugger wants us to skip these resets (e.g.
//!   during programming), or any other value if neither of those conditions
//!   hold.  If the token is valid, it it destroyed before `check` returns and
//!   the SP continues booting.
//! - Counter token, which is `COUNTER_TAG` if the subsequent word is expected
//!   to be a counter value.  The counter token is only written by the SP, and
//!   is destroyed in `check` if the decision is made to keep booting.
//! - Counter value indicating the number of resets; this starts at 1 and counts
//!   up from there.  The counter value is only written by the SP.
//! - Counter check word, which is `COUNTER_TAG` xor'd with the counter value.
//!   If the counter check word is incorrect, then the counter is reset to 0.
//!   The check word is only written by the SP.
#![no_std]

const COUNTER_TAG: u32 = 0x4e423d17;

unsafe extern "C" {
    static mut __REGION_DTCM_BASE: [u8; 0];
    static mut __REGION_DTCM_END: [u8; 0];
}

#[derive(Copy, Clone)]
pub enum MeasurementResult {
    /// [`measurement_token::SKIP`] was found in the token slot
    ///
    /// This indicates that an external source (typically an attached debugger)
    /// has instructed Hubris to skip the wait-for-measurement reboot dance.
    Skipped,
    /// [`measurement_token::VALID`] was found in the token slot
    ///
    /// This indicates that the Root of Trust has measured the SP image
    Valid { count: Option<u32> },
    /// Hubris exceeded its retry count and booted without being measured
    RetryCountExceeded,
}

impl MeasurementResult {
    /// Checks whether the result represents a valid measurement
    fn is_valid(&self) -> bool {
        match self {
            MeasurementResult::Valid { .. } => true,
            MeasurementResult::Skipped
            | MeasurementResult::RetryCountExceeded => false,
        }
    }
}

#[unsafe(no_mangle)]
pub static mut HUBRIS_MEASUREMENT_RESULT: Option<MeasurementResult> = None;

/// Check the measurement token, calling `reset_fn` to reset if needed
///
/// Calls `delay_and_reset` (which diverges) if no measurement is present and we
/// have not yet exceeded our retry count; otherwise, returns `true` if the
/// measurement is valid, or `false` if we exceeded `retry_count`.
///
/// `delay_and_reset` should include a delay, to give the RoT time to boot.
pub unsafe fn check(retry_count: u32, delay_and_reset: fn() -> !) -> bool {
    let ptr: *mut u32 = &raw mut __REGION_DTCM_BASE as *mut _;
    let end: *mut u32 = &raw mut __REGION_DTCM_END as *mut _;
    assert!(ptr == measurement_token::SP_ADDR);
    assert!(
        end as isize - ptr as isize >= 4 * core::mem::size_of::<u32>() as isize
    );

    // SAFETY: we trust the linker
    let (token, tag, counter, check) = unsafe {
        let token = core::ptr::read_volatile(ptr);
        let tag = core::ptr::read_volatile(ptr.wrapping_add(1));
        let counter = core::ptr::read_volatile(ptr.wrapping_add(2));
        let check = core::ptr::read_volatile(ptr.wrapping_add(3));
        (token, tag, counter, check)
    };

    let counter_valid = tag == COUNTER_TAG && tag ^ counter == check;
    let result = if token == measurement_token::VALID {
        // told that measurement was completed
        let count = if counter_valid { Some(counter) } else { None };
        Ok(MeasurementResult::Valid { count })
    } else if token == measurement_token::SKIP {
        // told to skip measuring
        Ok(MeasurementResult::Skipped)
    } else if !counter_valid {
        // no counter, so initialize it
        Err(0)
    } else if counter >= retry_count {
        // exceeded retry count, so boot
        Ok(MeasurementResult::RetryCountExceeded)
    } else {
        // we should reset the processor
        Err(counter)
    };

    match result {
        Ok(r) => {
            // Destroy the existing token and write our global variable for
            // later debugging.
            unsafe {
                core::ptr::write_volatile(ptr, 0);
                core::ptr::write_volatile(ptr.wrapping_add(1), 0);
                HUBRIS_MEASUREMENT_RESULT = Some(r);
            }
            r.is_valid()
        }
        Err(counter) => {
            // Increment the counter, then reset
            let next = counter + 1;
            unsafe {
                core::ptr::write_volatile(ptr.wrapping_add(1), COUNTER_TAG);
                core::ptr::write_volatile(ptr.wrapping_add(2), next);
                core::ptr::write_volatile(
                    ptr.wrapping_add(3),
                    next ^ COUNTER_TAG,
                );
            }
            delay_and_reset();
        }
    }
}
