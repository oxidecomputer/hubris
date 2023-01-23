// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(armv6m)]
use armv6m_atomic_hack::AtomicBoolExt;

/// A RefCell-style container that can be used in a static for cases where only
/// a single borrow needs to happen at any given time.
///
/// This only provides `mut` access because that's what we've needed so far. It
/// does _not_ provide the many-reader one-writer behavior of `RefCell`, only
/// the one-writer part.
#[derive(Default)]
pub struct StaticCell<T> {
    borrowed: AtomicBool,
    cell: UnsafeCell<T>,
}

impl<T> StaticCell<T> {
    /// Creates a `StaticCell` containing `contents`.
    pub const fn new(contents: T) -> Self {
        Self {
            borrowed: AtomicBool::new(false),
            cell: UnsafeCell::new(contents),
        }
    }

    /// Gets mutable access to the contents of `self`.
    ///
    /// If a `StaticRef` for `self` still exists anywhere in the program, this
    /// will panic.
    pub fn borrow_mut(&self) -> StaticRef<'_, T> {
        let already_borrowed = self.borrowed.swap(true, Ordering::Acquire);
        if already_borrowed {
            panic!();
        }
        // Safety: the check above ensures that we are not producing an aliasing
        // &mut to our contents.
        unsafe {
            StaticRef {
                contents: &mut *self.cell.get(),
                borrow: &self.borrowed,
            }
        }
    }
}

unsafe impl<T> Sync for StaticCell<T> where for<'a> &'a mut T: Send {}

pub struct StaticRef<'a, T> {
    contents: &'a mut T,
    borrow: &'a AtomicBool,
}

impl<'a, T> Drop for StaticRef<'a, T> {
    fn drop(&mut self) {
        self.borrow.store(false, Ordering::Release);
    }
}

impl<'a, T> core::ops::Deref for StaticRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &*self.contents
    }
}

impl<'a, T> core::ops::DerefMut for StaticRef<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.contents
    }
}
