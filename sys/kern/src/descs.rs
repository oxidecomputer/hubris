// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Descriptor types, used to statically define application resources.

pub(crate) const REGIONS_PER_TASK: usize = 8;

/// Indicates priority of a task.
///
/// Priorities are small numbers starting from zero. Numerically lower
/// priorities are more important, so Priority 0 is the most likely to be
/// scheduled, followed by 1, and so forth. (This keeps our logic simpler given
/// that the number of priorities can be reconfigured.)
///
/// Note that this type *deliberately* does not implement `PartialOrd`/`Ord`, to
/// keep us from confusing ourselves on whether `>` means numerically greater /
/// less important, or more important / numerically smaller.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
#[repr(transparent)]
pub struct Priority(pub u8);

impl Priority {
    /// Checks if `self` is strictly more important than `other`.
    ///
    /// This is easier to read than comparing the numeric values of the
    /// priorities, since lower numbers are more important.
    pub fn is_more_important_than(self, other: Self) -> bool {
        self.0 < other.0
    }
}

/// Record describing a single task.
#[derive(Clone, Debug)]
pub struct TaskDesc {
    /// Identifies memory regions this task has access to, with references into
    /// the `RegionDesc` table. If the task needs fewer than `REGIONS_PER_TASK`
    /// regions, it should use remaining entries to name a region that confers
    /// no access; by convention, this region is usually entry 0 in the table.
    /// (This is why we use pointers into a table, to avoid making many copies
    /// of that region.)
    pub regions: [&'static RegionDesc; REGIONS_PER_TASK],
    /// Address of the task's entry point. This is the first instruction that
    /// will be executed whenever the task is (re)started. It must be within one
    /// of the task's memory regions (the kernel *will* check this).
    pub entry_point: u32,
    /// Address of the task's initial stack pointer, to be loaded at (re)start.
    /// It must be pointing into or *just past* one of the task's memory
    /// regions (the kernel *will* check this).
    pub initial_stack: u32,
    /// Initial priority of this task.
    pub priority: u8,
    /// Collection of boolean flags controlling task behavior.
    pub flags: TaskFlags,
    /// Index of this task within the task table.
    ///
    /// This field is here as an optimization for the kernel entry sequences. It
    /// can contain an invalid index if you create an arbitrary invalid
    /// `TaskDesc`; this will cause the kernel to behave strangely (if the index
    /// is in range for the task table) or panic predictably (if not), but won't
    /// violate safety. The build system is careful to generate correct indices
    /// here.
    ///
    /// The index is a u16 to save space in the `TaskDesc` struct; in practice
    /// other factors limit us to fewer than `2**16` tasks.
    pub index: u16,
}

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug)]
    #[repr(transparent)]
    pub struct TaskFlags: u8 {
        const START_AT_BOOT = 1 << 0;
        const RESERVED = !1;
    }
}

/// Description of one memory region.
///
/// A memory region can be used by multiple tasks. This is mostly used to have
/// tasks share a no-access region (often index 0) in unused region slots, but
/// you could also use it for shared peripheral or RAM access.
///
/// Note that regions can overlap. This can be useful: for example, you can have
/// two regions pointing to the same area of the address space, but one
/// read-only and the other read-write.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct RegionDesc {
    /// Architecture-specific additional data to make context switch cheaper.
    /// Should be first in the struct to improve context switch code generation.
    pub arch_data: crate::arch::RegionDescExt,

    /// Address of start of region. The platform likely has alignment
    /// requirements for this; it must meet them. (For example, on ARMv7-M, it
    /// must be naturally aligned for the size.)
    pub base: u32,
    /// Size of region, in bytes. The platform likely has alignment requirements
    /// for this; it must meet them. (For example, on ARMv7-M, it must be a
    /// power of two greater than 16.)
    pub size: u32,
    /// Flags describing what can be done with this region.
    pub attributes: RegionAttributes,
}

impl RegionDesc {
    /// Tests whether `self` contains `addr`.
    pub fn contains(&self, addr: usize) -> bool {
        let next_addr = addr.wrapping_add(1);
        if next_addr < addr {
            return false;
        };
        let end = self.end_addr() as usize;

        (self.base as usize) <= addr && next_addr <= end
    }

    /// Compute the address one past the end of this region. Since we don't
    /// allow regions to butt up against the end of the address space, we can do
    /// that.
    pub fn end_addr(&self) -> u32 {
        // Wrapping add here avoids the overflow check, which is avoided by our
        // invariant that this not bump the end of the address space.
        self.base.wrapping_add(self.size)
    }

    pub fn dumpable(&self) -> bool {
        let ratts = self.attributes;

        ratts.contains(RegionAttributes::WRITE)
            && !ratts.contains(RegionAttributes::DEVICE)
    }
}

/// Compatibility with generic kernel algorithms defined in kerncore
impl kerncore::MemoryRegion for RegionDesc {
    #[inline(always)]
    fn contains(&self, addr: usize) -> bool {
        self.contains(addr)
    }

    #[inline(always)]
    fn base_addr(&self) -> usize {
        self.base as usize
    }

    #[inline(always)]
    fn end_addr(&self) -> usize {
        self.end_addr() as usize
    }
}

// This is defined outside the bitflags! macro so that we can write our own
// const constructor fn, below.
#[repr(transparent)]
#[derive(Copy, Clone, Debug)]
pub struct RegionAttributes(u32);

bitflags::bitflags! {
    impl RegionAttributes: u32 {
        /// Region can be read by tasks that include it.
        const READ = 1 << 0;
        /// Region can be written by tasks that include it.
        const WRITE = 1 << 1;
        /// Region can contain executable code for tasks that include it.
        const EXECUTE = 1 << 2;
        /// Region contains memory mapped registers. This affects cache behavior
        /// on devices that include it, and discourages the kernel from using
        /// `memcpy` in the region.
        const DEVICE = 1 << 3;
        /// Region can be used for DMA or communication with other processors.
        /// This heavily restricts how this memory can be cached and will hurt
        /// performance if overused.
        ///
        /// This is ignored for `DEVICE` memory, which is already not cached.
        const DMA = 1 << 4;

        const RESERVED = !((1 << 5) - 1);
    }
}

impl RegionAttributes {
    pub const unsafe fn from_bits_unchecked(bits: u32) -> Self {
        Self(bits)
    }
}
