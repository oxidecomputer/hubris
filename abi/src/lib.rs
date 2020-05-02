//! Kernel ABI definitions, shared between kernel and applications.

#![no_std]

use zerocopy::{AsBytes, FromBytes, Unaligned};

/// Magic number that appears at the start of an application header (`App`) to
/// reassure the kernel that it is not reading uninitialized Flash.
pub const CURRENT_APP_MAGIC: u32 = 0x1DE_fa7a1;

/// Number of region slots in a `TaskDesc` record. Needs to be less or equal to
/// than the number of regions in the MPU; may be less to improve context switch
/// performance. (Though note that changing this alters the ABI.)
pub const REGIONS_PER_TASK: usize = 8;

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

/// Application header, read by the kernel to load the application.
///
/// One copy of this appears in Flash next to the kernel, with the other types
/// of records following immediately.
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
    /// Bitmask to post to task 0 when any task faults.
    pub fault_notification: u32,

    /// Reserved expansion space; pads this structure out to 32 bytes. You will
    /// need to adjust this when you add fields above.
    pub zeroed_expansion_space: [u8; 32 - (5 * 4)],
}

/// Record describing a single task.
#[derive(Clone, Debug, FromBytes)]
#[repr(C)]
pub struct TaskDesc {
    /// Identifies memory regions this task has access to, by index in the
    /// `RegionDesc` table. If the task needs fewer than `REGIONS_PER_TASK`
    /// regions, it should use remaining entries to name a region that confers
    /// no access; by convention, this region is usually entry 0 in the table.
    ///
    /// Note: because these region indices are 8 bits, this is going to get
    /// restrictive in applications that approach 128 tasks.
    pub regions: [u8; REGIONS_PER_TASK],
    /// Address of the task's entry point. This is the first instruction that
    /// will be executed whenever the task is (re)started. It must be within one
    /// of the task's memory regions (the kernel *will* check this).
    pub entry_point: u32,
    /// Address of the task's initial stack pointer, to be loaded at (re)start.
    /// It must be pointing into or *just past* one of the task's memory
    /// regions (the kernel *will* check this).
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
///
/// A memory region can be used by multiple tasks. This is mostly used to have
/// tasks share a no-access region (often index 0) in unused region slots, but
/// you could also use it for shared peripheral or RAM access.
///
/// Note that regions can overlap. This can be useful: for example, you can have
/// two regions pointing to the same area of the address space, but one
/// read-only and the other read-write.
#[derive(Clone, Debug, FromBytes)]
#[repr(C)]
pub struct RegionDesc {
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
    /// Reserved word, must be zero.
    pub reserved_zero: u32,
}

bitflags::bitflags! {
    #[derive(FromBytes)]
    #[repr(transparent)]
    pub struct RegionAttributes: u32 {
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
        /// Allow the borrower to read this memory.
        const READ = 1 << 0;
        /// Allow the borrower to write this memory.
        const WRITE = 1 << 1;
    }
}

/// Response code returned by the kernel if the peer died or was restarted.
pub const DEAD: u32 = !0;

/// Response code returned by the kernel if a lender has defected.
pub const DEFECT: u32 = 1;

/// State used to make scheduling decisions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TaskState {
    /// Task is healthy and can be scheduled subject to the `SchedState`
    /// requirements.
    Healthy(SchedState),
    /// Task has been stopped by a fault and must not be scheduled without
    /// intervention.
    Faulted {
        /// Information about the fault.
        fault: FaultInfo,
        /// Record of the previous healthy state at the time the fault was
        /// taken.
        original_state: SchedState,
    },
}

impl Default for TaskState {
    fn default() -> Self {
        TaskState::Healthy(SchedState::Stopped)
    }
}

/// Scheduler parameters for a healthy task.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SchedState {
    /// This task is ignored for scheduling purposes.
    Stopped,
    /// This task could be scheduled on the CPU.
    Runnable,
    /// This task is blocked waiting to deliver a message to the given task.
    InSend(usize),
    /// This task is blocked waiting for a reply from the given task.
    InReply(usize),
    /// This task is blocked waiting for messages, either from any source
    /// (`None`) or from a particular sender only.
    InRecv(Option<usize>),
}

/// A record describing a fault taken by a task.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FaultInfo {
    /// The task has violated memory access rules. This may have come from a
    /// memory protection fault while executing the task (in the case of
    /// `source` `User`), or from checks on kernel syscall arguments (`source`
    /// `Kernel`).
    MemoryAccess {
        /// Problematic address that the task accessed, or asked the kernel to
        /// access. This is `Option` because there are cases of processor
        /// protection faults that don't provide a precise address.
        address: Option<usize>,
        /// Origin of the fault.
        source: FaultSource,
    },
    /// Arguments passed to a syscall were invalid. TODO: this should become
    /// more descriptive, it's a placeholder.
    SyscallUsage(UsageError),
    /// A task has explicitly aborted itself with a panic.
    Panic,
}

impl From<UsageError> for FaultInfo {
    fn from(e: UsageError) -> Self {
        Self::SyscallUsage(e)
    }
}

/// A kernel-defined fault, arising from how a user task behaved.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// A program used an undefined syscall number.
    BadSyscallNumber,
    /// A program specified a slice as a syscall argument, but the slice is
    /// patently invalid: it is either unaligned for its type, or it is
    /// expressed such that it would wrap around the end of the address space.
    /// Neither of these conditions is ever legal, so this represents a
    /// malfunction in the caller.
    InvalidSlice,
    /// A program named a task ID that will never be valid, as it's out of
    /// range.
    TaskOutOfRange,
    LeaseOutOfRange,
    OffsetOutOfRange,
    NoIrq,
    BadKernelMessage,
}

/// Origin of a fault.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FaultSource {
    /// User code did something that was intercepted by the processor.
    User,
    /// User code asked the kernel to do something bad on its behalf.
    Kernel,
}
