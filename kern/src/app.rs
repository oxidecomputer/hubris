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
        let self_end = self.base.wrapping_add(self.size).wrapping_sub(1) as usize;
        let slice_end = slice.last_byte_addr();

        self_end >= slice.base_addr() && slice_end >= self.base as usize
    }
}
