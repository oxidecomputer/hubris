// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Application description and startup.
//!
//! An "application" here is the entire collection of tasks and configuration
//! that customize the generic kernel.
//!
//! Most of the interesting types in this module are sourced from the `abi`
//! crate, where they can be shared with app code.

// Re-export ABI types.
pub use abi::*;

/// Adds kernel-specific operations to `abi::RegionDesc`. Not intended to be
/// implemented by other types.
pub trait RegionDescExt {
    /// Tests whether `slice` is fully enclosed by this region.
    fn covers<T>(&self, slice: &crate::umem::USlice<T>) -> bool;
}

impl RegionDescExt for abi::RegionDesc {
    /// Tests whether `slice` is fully enclosed by this region.
    fn covers<T>(&self, slice: &crate::umem::USlice<T>) -> bool {
        // We don't allow regions to butt up against the end of the address
        // space, so we can compute our off-by-one end address as follows:
        let self_end = self.base.wrapping_add(self.size) as usize;

        (self.base as usize) <= slice.base_addr()
            && slice.end_addr() <= self_end
    }
}
