// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Kernel atomic type support.

use core::sync::atomic::Ordering;

/// An atomic type with the operations we need in the kernel.
///
/// Rust removes certain atomic operations from the `core::sync::atomic` API on
/// platforms, like Cortex-M0, that don't support them. The decision to remove
/// these features in the libcore sources is controlled by some `cfg`s that
/// we're not allowed to look at -- they've been unstable forever. Not clear how
/// upstream expects us to handle those cases, but, hey.
///
/// This trait describes an atomic type with the complement of atomic ops that
/// we need to make the kernel work. We can then implement it to call through to
/// the native versions (on M3 and later) or use hacks (on M0).
///
/// Implementations of this trait are in the `arch::whatever` module for the
/// target architecture.
pub(crate) trait AtomicExt {
    type Primitive;
    fn swap_polyfill(
        &self,
        value: Self::Primitive,
        ordering: Ordering,
    ) -> Self::Primitive;
}
