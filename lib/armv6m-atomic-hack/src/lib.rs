// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Provides fake atomic read-modify-write operations for situations where you
//! _really_ know what you're doing.
//!
//! Pulling this trait in can cause code written for ARMv7-M and later machines,
//! which have atomic read-modify-write operations, to compile on ARMv6-M. This
//! is, in general, not safe: the program wanted an atomic read-modify-write and
//! you're faking it with a non-atomic sequence. However, in our _specific_ case
//! on Hubris, we can do this safely because
//!
//! 1. Tasks are isolated.
//! 2. Tasks are single-threaded.
//! 3. ISRs do not access memory shared with tasks.
//!
//! If any of those three points is wrong in your case, do not use these, it
//! will go badly for you.
//!
//! Everything in this crate is conditional on the `armv6m` config, which means
//! (1) it will only work inside the Hubris build system and (2) accidentally
//! including it on armv7m or later won't pull in the bogus implementations.
//!
//! # Why not just disable interrupts
//!
//! Because (1) we can't from unprivileged mode and (2) we don't out of
//! principle anyway.

#![no_std]

#[cfg(armv6m)]
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[cfg(armv6m)]
pub trait AtomicU32Ext {
    fn swap(&self, val: u32, order: Ordering) -> u32;
    fn fetch_add(&self, val: u32, order: Ordering) -> u32;
    fn fetch_sub(&self, val: u32, order: Ordering) -> u32;
}

#[cfg(armv6m)]
impl AtomicU32Ext for AtomicU32 {
    #[inline]
    fn swap(&self, val: u32, order: Ordering) -> u32 {
        let rv = self.load(order);
        self.store(val, order);
        rv
    }

    #[inline]
    fn fetch_add(&self, val: u32, order: Ordering) -> u32 {
        let rv = self.load(order);
        self.store(rv + val, order);
        rv
    }

    #[inline]
    fn fetch_sub(&self, val: u32, order: Ordering) -> u32 {
        let rv = self.load(order);
        self.store(rv - val, order);
        rv
    }
}

#[cfg(armv6m)]
pub trait AtomicBoolExt {
    fn swap(&self, val: bool, order: Ordering) -> bool;
}

#[cfg(armv6m)]
impl AtomicBoolExt for AtomicBool {
    #[inline]
    fn swap(&self, val: bool, order: Ordering) -> bool {
        let rv = self.load(order);
        self.store(val, order);
        rv
    }
}
