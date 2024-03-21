// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An implementation to support the `critical-section` crate within a Hubris
//! user task.
//!
//! Hubris is careful never to introduce non-local or asynchronous control flow
//! into a program, and doesn't have threads. This means that, within the
//! context of a task, we don't actually need to generate any code to implement
//! a critical section --- they happen naturally.
//!
//! You might want to opt out of this implementation if you're doing something
//! very weird with shared memory. By default, it is hard to do that in Hubris,
//! but it is possible, and we'll assume you know what you've signed yourself up
//! for should you choose to do it.

use critical_section::RawRestoreState;

struct HubrisCriticalSection;
critical_section::set_impl!(HubrisCriticalSection);

unsafe impl critical_section::Impl for HubrisCriticalSection {
    #[inline(always)]
    unsafe fn acquire() -> RawRestoreState {
        // No action required.
    }

    #[inline(always)]
    unsafe fn release(_token: RawRestoreState) {
        // Again, no action required.
    }
}
