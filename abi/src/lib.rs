//! Kernel ABI definitions, shared between kernel and applications.

#![no_std]

use zerocopy::{AsBytes, FromBytes, Unaligned};

pub const CURRENT_APP_MAGIC: u32 = 0x1DE_fa7a1;
pub const REGIONS_PER_TASK: usize = 8;

/// Indicates priority of a task.
///
/// Priorities are small numbers starting from zero. Numerically lower
/// priorities are more important, so Priority 0 is the most likely to be
/// scheduled, followed by 1, and so forth. (This keeps our logic simpler given
/// that the number of priorities can be reconfigured.)
#[derive(
    Copy, Clone, Debug, Eq, PartialEq, FromBytes, AsBytes, Unaligned, Default,
)]
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

#[derive(Clone, Debug, FromBytes)]
#[repr(C)]
pub struct App {
    /// Reassures the kernel that it is dealing with this kind of an app struct.
    /// Should have the value `CURRENT_APP_MAGIC`.
    pub magic: u32,
    /// Number of tasks. This many `TaskDesc` records will immediately follow
    /// the app header.
    pub task_count: u32,
    /// Number of memory regions in the address space layout. This many
    /// `RegionDesc` records will immediately follow the `TaskDesc` array.
    pub region_count: u32,
    /// Number of interrupt response records that will follow the `RegionDesc`
    /// records.
    pub irq_count: u32,

    /// Reserved expansion space; pads this structure out to 32 bytes. You will
    /// need to adjust this when you add fields above.
    pub zeroed_expansion_space: [u8; 32 - (4 * 4)],
}

#[derive(Clone, Debug, FromBytes)]
#[repr(C)]
pub struct TaskDesc {
    /// Identifies memory regions this task has access to, by index in the
    /// `RegionDesc` table. If the task needs fewer than `REGIONS_PER_TASK`
    /// regions, it should use remaining entries to name a region that confers
    /// no access; by convention, this region is usually entry 0 in the table.
    pub regions: [u8; REGIONS_PER_TASK],
    /// Address of the task's entry point. This is the first instruction that
    /// will be executed whenever the task is (re)started. It must be within one
    /// of the task's memory regions.
    pub entry_point: u32,
    /// Address of the task's initial stack pointer, to be loaded at (re)start.
    /// It must be pointing into or *just past* one of the task's memory
    /// regions.
    pub initial_stack: u32,
    /// Initial priority of this task.
    pub priority: u32,
    /// Collection of boolean flags controlling task behavior.
    pub flags: TaskFlags,
}

bitflags::bitflags! {
    #[derive(FromBytes)]
    #[repr(transparent)]
    pub struct TaskFlags: u32 {
        const START_AT_BOOT = 1 << 0;
        const RESERVED = !1;
    }
}

/// Description of one memory region.
#[derive(Clone, Debug, FromBytes)]
#[repr(C)]
pub struct RegionDesc {
    /// Address of start of region. The platform likely has alignment
    /// requirements for this; it must meet them.
    pub base: u32,
    /// Size of region, in bytes. The platform likely has alignment requirements
    /// for this; it must meet them.
    pub size: u32,
    /// Flags describing what can be done with this region.
    pub attributes: RegionAttributes,
    /// Reserved word, must be zero.
    pub reserved_zero: u32,
}

bitflags::bitflags! {
    #[derive(FromBytes)]
    #[repr(transparent)]
    pub struct RegionAttributes: u32 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        const EXECUTE = 1 << 2;
        const RWX = 0b111;
        const DEVICE = 1 << 3;
        const RESERVED = !((1 << 4) - 1);
    }
}

/// Description of one interrupt response.
#[derive(Clone, Debug, FromBytes)]
#[repr(C)]
pub struct Interrupt {
    /// Which interrupt number is being hooked.
    pub irq: u32,
    /// Which task to notify, by index.
    pub task: u32,
    /// Which notification bits to set.
    pub notification: u32,
}

/// Structure describing a lease in task memory.
///
/// At SEND, the task gives us the base and length of a section of memory that
/// it *claims* contains structs of this type.
#[derive(Copy, Clone, Debug, FromBytes)]
#[repr(C)]
pub struct ULease {
    /// Lease attributes.
    pub attributes: LeaseAttributes,
    /// Base address of leased memory. This is equivalent to the base address
    /// field in `USlice`, but isn't represented as a `USlice` because we leave
    /// the internal memory representation of `USlice` out of the ABI.
    pub base_address: usize,
    /// Length of leased memory, in bytes.
    pub length: usize,
}

bitflags::bitflags! {
    #[derive(FromBytes)]
    #[repr(transparent)]
    pub struct LeaseAttributes: u32 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
    }
}

/// Response code returned by the kernel if the peer died or was restarted.
pub const DEAD: u32 = !0;

/// Response code returned by the kernel if a lender has defected.
pub const DEFECT: u32 = 1;
