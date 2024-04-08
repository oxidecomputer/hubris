// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
use armv6m_atomic_hack::AtomicBoolExt;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

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
        let already_borrowed =
            AtomicBoolExt::swap(&self.borrowed, true, Ordering::Acquire);
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

/// A simpler variant of [`StaticCell`], which may be claimed only a single
/// time.
///
/// Because the value may only be claimed once, and any repeated call to
/// [`ClaimOnceCell::claim`] will panic, the method may return a `&'static mut
/// T` rather than a `StaticRef<'static, T>` guard. Once the value has been
/// claimed, no other reference to it may ever be created, so a type
/// implementing `Drop` is not required to release it.
pub struct ClaimOnceCell<T> {
    taken: AtomicBool,
    cell: UnsafeCell<T>,
}

// Safety: because a `ClaimOnceCell` may only create a single mutable reference to
// the inner value a single time, it can implement `Sync` freely, as the inner
// `UnsafeCell`'s value cannot be mutably aliased.
unsafe impl<T> Sync for ClaimOnceCell<T> where for<'a> &'a T: Send {}

impl<T> ClaimOnceCell<T> {
    /// Returns a new `ClaimOnceCell` containing the provided `value`.
    ///
    /// The returned `ClaimOnceCell` will not have yet been claimed, and the
    /// [`ClaimOnceCell::claim`] method may be called to claim exclusive access
    /// to the contents.
    pub const fn new(value: T) -> Self {
        Self {
            taken: AtomicBool::new(false),
            cell: UnsafeCell::new(value),
        }
    }

    /// Claims the value inside this cell, if it has not already been claimed,
    /// returning a `&mut T` referencing the value.
    ///
    /// If this method has already been called, subsequent calls will panic.
    #[track_caller] // Let's get useful panic locations
    #[must_use = "claiming a `ClaimOnceCell` and not accessing it will render \
         it permanently unusable, as it will have already been claimed!"]
    pub fn claim(&self) -> &mut T {
        if self.taken.swap(true, Ordering::Relaxed) {
            panic!();
        }

        unsafe {
            // Safety: dereferencing a raw pointer is unsafe as the value may
            // be aliased. However, because `ClaimOnceCell::claim` is the
            // *only*  way to access the inner value, and the `taken` bool
            // ensures it is only ever called once, we know that this raw
            // pointer does not point to aliased data.
            &mut *self.cell.get()
        }
    }
}
