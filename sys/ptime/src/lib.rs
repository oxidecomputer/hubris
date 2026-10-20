// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Precise time
//!
//! Currently only used for profiling

#![no_std]

use core::sync::atomic::{AtomicPtr, Ordering};

#[derive(Debug, Clone, Copy)]
pub struct Instant(pub u64);
#[derive(Debug, Clone, Copy)]
pub struct Duration(pub u64);

impl Duration {
    pub const ZERO: Self = Self(0);
}

/// "Now" Function, returns the current time in ticks as an [`Instant`].
pub type NowFunc = fn() -> Instant;
/// "Timekeep" function, called periodically to keep the coarse time up to date.
pub type TimeKeepFunc = fn();
/// "Tickrate" function, returns the number of timer ticks per second.
pub type TickRateFunc = fn() -> u32;

/// VTable of functions for precision timer. Obtained with `ptimer()`, and
/// provided using `set_ptime_vtable()`.
pub struct PTimeVTable {
    pub now: NowFunc,
    pub timekeep: TimeKeepFunc,
    pub tickrate: TickRateFunc,
}

static PTIME_VTABLE: AtomicPtr<PTimeVTable> =
    AtomicPtr::new(core::ptr::null_mut());

/// Set the ptime vtable
pub fn set_ptime_vtable(vtable: &'static PTimeVTable) {
    let vtable: *const PTimeVTable = vtable;
    let vtable: *mut PTimeVTable = vtable.cast_mut();
    PTIME_VTABLE.store(vtable, Ordering::Release)
}

pub fn ptimer() -> Option<&'static PTimeVTable> {
    let vtable: *mut PTimeVTable = PTIME_VTABLE.load(Ordering::Acquire);
    let vtable: *const PTimeVTable = vtable.cast_const();
    unsafe { vtable.as_ref() }
}
