// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
/// Wraps a T which is expected to be constant at runtime but may change
/// after compilation.
///
/// A static or const T is considered immutable by the compiler so it may
/// constant-fold the value (as known at compile-time).  If the T is
/// expected to be modified between compilation and runtime, it does not
/// meet the compiler's definition of immutable but the compiler doesn't
/// know that without some help.  https://crates.io/crates/vcell seems like
/// an available solution but it also provides inner mutability via
/// UnsafeCell which causes the compiler to move it from .rodata linker
/// section to .data and thus consuming slightly more RAM than necessary.
/// Instead, VolatileConst provides only a copying getter which keeps the
/// variable in .rodata and more accurately reflects that this value is
/// expected to be immutable at runtime.
#[repr(transparent)]
pub struct VolatileConst<T> {
    value: T,
}

impl<T> VolatileConst<T> {
    /// Creates a new `VolatileConst` containing the given value
    pub const fn new(value: T) -> Self {
        Self { value }
    }

    /// Returns a copy of the contained value
    #[inline(always)]
    pub fn get(&self) -> T
    where
        T: Copy,
    {
        unsafe { core::ptr::read_volatile(&self.value) }
    }

    /// Returns a raw pointer to the underlying data in the cell
    ///
    /// Directly reading through this pointer at runtime is an error.
    pub const fn as_ptr(&self) -> *const T {
        &self.value
    }
}
