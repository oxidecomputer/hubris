// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Kernel ABI definitions, shared between kernel and applications.

#![no_std]

use serde::{Deserialize, Serialize};
use zerocopy::{AsBytes, FromBytes};

/// Names a particular incarnation of a task.
///
/// A `TaskId` combines two fields, a task index (which can be predicted at
/// compile time) and a task generation number. The generation number begins
/// counting at zero and wraps on overflow. Critically, the generation number of
/// a task is incremented when it is restarted. Attempts to correspond with a
/// task using an outdated generation number will return `DEAD`. This helps
/// provide assurance that your peer has not lost its memory between steps of a
/// multi-step IPC sequence.
///
/// If the IPC can be retried against a fresh instance of the peer, it's
/// reasonable to simply increment the generation number and try again, using
/// `TaskId::next_generation`.
///
/// The task index is in the lower `TaskId::INDEX_BITS` bits, while the
/// generation is in the remaining top bits.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskId(pub u16);

impl TaskId {
    /// The all-ones `TaskId` is reserved to represent the "virtual kernel
    /// task."
    pub const KERNEL: Self = Self(!0);

    /// Reserved TaskId for an unbound userlib::task_slot!()
    pub const UNBOUND: Self = Self(Self::INDEX_MASK - 1);

    /// Number of bits in a `TaskId` used to represent task index, rather than
    /// generation number. This must currently be 15 or smaller.
    pub const INDEX_BITS: u32 = 10;

    /// Derived mask of the index bits portion.
    pub const INDEX_MASK: u16 = (1 << Self::INDEX_BITS) - 1;

    /// Fabricates a `TaskId` for a known index and generation number.
    pub const fn for_index_and_gen(index: usize, gen: Generation) -> Self {
        TaskId(
            (index as u16 & Self::INDEX_MASK)
                | (gen.0 as u16) << Self::INDEX_BITS,
        )
    }

    /// Extracts the index part of this ID.
    pub fn index(&self) -> usize {
        usize::from(self.0 & Self::INDEX_MASK)
    }

    /// Extracts the generation part of this ID.
    pub fn generation(&self) -> Generation {
        Generation((self.0 >> Self::INDEX_BITS) as u8)
    }

    pub fn next_generation(self) -> Self {
        Self::for_index_and_gen(self.index(), self.generation().next())
    }
}

/// Type used to track generation numbers.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
#[repr(transparent)]
pub struct Generation(u8);

impl Generation {
    pub const ZERO: Self = Self(0);

    pub fn next(self) -> Self {
        const MASK: u16 = 0xFFFF << TaskId::INDEX_BITS >> TaskId::INDEX_BITS;
        Generation(self.0.wrapping_add(1) & MASK as u8)
    }
}

impl From<u8> for Generation {
    fn from(x: u8) -> Self {
        Self(x)
    }
}

/// Newtype wrapper for an interrupt index
#[derive(
    Copy,
    Clone,
    Debug,
    FromBytes,
    Serialize,
    Deserialize,
    Hash,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
)]
#[repr(transparent)]
pub struct InterruptNum(pub u32);
impl phash::PerfectHash for InterruptNum {
    fn phash(&self, v: u32) -> usize {
        self.0.wrapping_mul(v) as usize
    }
}
impl InterruptNum {
    pub const fn invalid() -> Self {
        Self(u32::MAX)
    }
    pub fn is_valid(&self) -> bool {
        self.0 != u32::MAX
    }
}

/// Struct containing the task which waits for an interrupt, and the expected
/// notification mask associated with the IRQ.
#[derive(
    Copy,
    Clone,
    Debug,
    FromBytes,
    Serialize,
    Deserialize,
    Hash,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
)]
pub struct InterruptOwner {
    /// Which task to notify, by index.
    pub task: u32,
    /// Which notification bits to set.
    pub notification: u32,
}
impl phash::PerfectHash for InterruptOwner {
    fn phash(&self, v: u32) -> usize {
        self.task
            .wrapping_mul(v)
            .wrapping_add(self.notification.wrapping_mul(!v)) as usize
    }
}
impl InterruptOwner {
    pub const fn invalid() -> Self {
        Self {
            task: u32::MAX,
            notification: 0,
        }
    }
    pub fn is_valid(&self) -> bool {
        !(self.task == u32::MAX && self.notification == 0)
    }
}

/// Description of one interrupt response.
#[derive(Clone, Debug, FromBytes, Serialize, Deserialize)]
pub struct Interrupt {
    /// Which interrupt number is being hooked.
    pub irq: InterruptNum,
    /// The owner of this interrupt.
    pub owner: InterruptOwner,
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
    pub base_address: u32,
    /// Length of leased memory, in bytes.
    pub length: u32,
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

pub const FIRST_DEAD_CODE: u32 = 0xffff_ff00;

/// Response code returned by the kernel if the peer died or was restarted.
///
/// This always has the top 24 bits set to 1, with the `generation` in the
/// bottom 8 bits.
pub const fn dead_response_code(new_generation: Generation) -> u32 {
    FIRST_DEAD_CODE | new_generation.0 as u32
}

/// Utility for checking whether a code indicates that the peer was restarted
/// and extracting the generation if it is.
pub const fn extract_new_generation(code: u32) -> Option<Generation> {
    if (code & FIRST_DEAD_CODE) == FIRST_DEAD_CODE {
        Some(Generation(code as u8))
    } else {
        None
    }
}

/// Response code returned by the kernel if a lender has defected.
pub const DEFECT: u32 = 1;

/// State used to make scheduling decisions.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
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

impl TaskState {
    /// Checks if a task in this state is ready to accept a message sent by
    /// `caller`. This will return `true` if the state is an open receive, or a
    /// closed receive naming the caller specifically; otherwise, it will return
    /// `false`.
    pub fn can_accept_message_from(&self, caller: TaskId) -> bool {
        if let TaskState::Healthy(SchedState::InRecv(peer)) = self {
            peer.is_none() || peer == &Some(caller)
        } else {
            false
        }
    }

    /// Checks if a task in this state is trying to deliver a message to
    /// `target`.
    pub fn is_sending_to(&self, target: TaskId) -> bool {
        self == &TaskState::Healthy(SchedState::InSend(target))
    }

    /// Checks if a task in this state can be unblocked with a notification.
    pub fn can_accept_notification(&self) -> bool {
        if let TaskState::Healthy(SchedState::InRecv(p)) = self {
            p.is_none() || p == &Some(TaskId::KERNEL)
        } else {
            false
        }
    }
}

impl Default for TaskState {
    fn default() -> Self {
        TaskState::Healthy(SchedState::Stopped)
    }
}

/// Scheduler parameters for a healthy task.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum SchedState {
    /// This task is ignored for scheduling purposes.
    Stopped,
    /// This task could be scheduled on the CPU.
    Runnable,
    /// This task is blocked waiting to deliver a message to the given task.
    InSend(TaskId),
    /// This task is blocked waiting for a reply from the given task.
    InReply(TaskId),
    /// This task is blocked waiting for messages, either from any source
    /// (`None`) or from a particular sender only.
    InRecv(Option<TaskId>),
}

impl From<SchedState> for TaskState {
    fn from(s: SchedState) -> Self {
        Self::Healthy(s)
    }
}

/// A record describing a fault taken by a task.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum FaultInfo {
    /// The task has violated memory access rules. This may have come from a
    /// memory protection fault while executing the task (in the case of
    /// `source` `User`), from overflowing a stack, or from checks on kernel
    /// syscall arguments (`source` `Kernel`).
    MemoryAccess {
        /// Problematic address that the task accessed, or asked the kernel to
        /// access. This is `Option` because there are cases of processor
        /// protection faults that don't provide a precise address.
        address: Option<u32>,
        /// Origin of the fault.
        source: FaultSource,
    },
    /// A task has overflowed its stack. We can always determine the bad
    /// stack address, but we can't determine the PC
    StackOverflow { address: u32 },
    /// A task has induced a bus error
    BusError {
        address: Option<u32>,
        source: FaultSource,
    },
    /// Divide-by-zero
    DivideByZero,
    /// Attempt to execute non-executable memory
    IllegalText,
    /// Execution of an illegal instruction
    IllegalInstruction,
    /// Other invalid operation, with 32-bit code. We use this for faults that
    /// aren't general across architectures or may not have enough diagnosis
    /// information. The code is architecture-specific.
    ///
    /// - ARMv7/8-M: used for faults not otherwise enumerated in this type; the
    ///   code is the bits of the Configurable Fault Status Register.
    /// - ARMv6-M: used for all faults, as v6 doesn't distinguish faults. The
    ///   code is always 0.
    InvalidOperation(u32),
    /// Arguments passed to a syscall were invalid. TODO: this should become
    /// more descriptive, it's a placeholder.
    SyscallUsage(UsageError),
    /// A task has explicitly aborted itself with a panic.
    Panic,
    /// A fault has been injected into this task by another task
    Injected(TaskId),
    /// A fault has been delivered by a server task.
    FromServer(TaskId, ReplyFaultReason),
}

/// We're using an explicit `TryFrom` impl for `Sysnum` instead of
/// `FromPrimitive` because the kernel doesn't currently depend on `num-traits`
/// and this seems okay.
impl core::convert::TryFrom<u32> for ReplyFaultReason {
    type Error = ();

    fn try_from(x: u32) -> Result<Self, Self::Error> {
        match x {
            0 => Ok(Self::UndefinedOperation),
            1 => Ok(Self::BadMessageSize),
            2 => Ok(Self::BadMessageContents),
            3 => Ok(Self::BadLeases),
            4 => Ok(Self::ReplyBufferTooSmall),
            5 => Ok(Self::AccessViolation),
            _ => Err(()),
        }
    }
}

impl From<UsageError> for FaultInfo {
    fn from(e: UsageError) -> Self {
        Self::SyscallUsage(e)
    }
}

/// A kernel-defined fault, arising from how a user task behaved.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
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
    /// A program named a valid task ID, but attempted to perform an operation
    /// on it that is illegal or otherwise forbidden.
    IllegalTask,
    LeaseOutOfRange,
    OffsetOutOfRange,
    NoIrq,
    BadKernelMessage,
    BadReplyFaultReason,
    NotSupervisor,
}

/// Origin of a fault.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum FaultSource {
    /// User code did something that was intercepted by the processor.
    User,
    /// User code asked the kernel to do something bad on its behalf.
    Kernel,
}

/// Reasons a server might cite when using the `REPLY_FAULT` syscall.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum ReplyFaultReason {
    /// The message indicated some operation number that is unknown to the
    /// server -- which almost certainly indicates that the client intended the
    /// message for a different kind of server.
    UndefinedOperation = 0,
    /// The message sent by the client had the wrong size to even attempt
    /// parsing by the server -- either too short or too long. (Because most
    /// messages are fixed size, it currently doesn't seem useful to distinguish
    /// between too-short and too-long.)
    BadMessageSize = 1,
    /// The server attempted to parse the message, and couldn't. This may
    /// indicate an enum with an illegal value, or a more nuanced error on
    /// operations that use serde encoding.
    BadMessageContents = 2,
    /// The client did not provide the leases required for the operation, or
    /// provided them with the wrong attributes.
    BadLeases = 3,
    /// The client did not provide a reply buffer large enough to receive the
    /// server's reply, despite this information being implied by the IPC
    /// protocol.
    ReplyBufferTooSmall = 4,

    /// Application-defined: The client attempted to operate on a resource that
    /// is not available to them due to mandatory access control or other type
    /// of access validation.
    AccessViolation = 5,
}

/// Enumeration of syscall numbers.
#[repr(u32)]
pub enum Sysnum {
    Send = 0,
    Recv = 1,
    Reply = 2,
    SetTimer = 3,
    BorrowRead = 4,
    BorrowWrite = 5,
    BorrowInfo = 6,
    IrqControl = 7,
    Panic = 8,
    GetTimer = 9,
    RefreshTaskId = 10,
    Post = 11,
    ReplyFault = 12,
}

/// We're using an explicit `TryFrom` impl for `Sysnum` instead of
/// `FromPrimitive` because the kernel doesn't currently depend on `num-traits`
/// and this seems okay.
impl core::convert::TryFrom<u32> for Sysnum {
    type Error = ();

    fn try_from(x: u32) -> Result<Self, Self::Error> {
        match x {
            0 => Ok(Self::Send),
            1 => Ok(Self::Recv),
            2 => Ok(Self::Reply),
            3 => Ok(Self::SetTimer),
            4 => Ok(Self::BorrowRead),
            5 => Ok(Self::BorrowWrite),
            6 => Ok(Self::BorrowInfo),
            7 => Ok(Self::IrqControl),
            8 => Ok(Self::Panic),
            9 => Ok(Self::GetTimer),
            10 => Ok(Self::RefreshTaskId),
            11 => Ok(Self::Post),
            12 => Ok(Self::ReplyFault),
            _ => Err(()),
        }
    }
}

/// A region to be dumped from a task
#[derive(Copy, Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct TaskDumpRegion {
    pub base: u32,
    pub size: u32,
}

/// Representation of kipc numbers
pub enum Kipcnum {
    ReadTaskStatus = 1,
    RestartTask = 2,
    FaultTask = 3,
    ReadImageId = 4,
    Reset = 5,
    ReadCaboosePos = 6,
    GetTaskDumpRegion = 7,
    ReadTaskDumpRegion = 8,
}

impl core::convert::TryFrom<u16> for Kipcnum {
    type Error = ();

    fn try_from(x: u16) -> Result<Self, Self::Error> {
        match x {
            1 => Ok(Self::ReadTaskStatus),
            2 => Ok(Self::RestartTask),
            3 => Ok(Self::FaultTask),
            4 => Ok(Self::ReadImageId),
            5 => Ok(Self::Reset),
            6 => Ok(Self::ReadCaboosePos),
            7 => Ok(Self::GetTaskDumpRegion),
            8 => Ok(Self::ReadTaskDumpRegion),
            _ => Err(()),
        }
    }
}

pub const HEADER_MAGIC: u32 = 0x64_CE_D6_CA;
pub const CABOOSE_MAGIC: u32 = 0xCAB0_005E;

/// TODO: Add hash for integrity check
/// Later this will also be a signature block
#[repr(C)]
#[derive(Default, AsBytes, FromBytes)]
pub struct ImageHeader {
    pub magic: u32,
    pub total_image_len: u32,
    pub _pad: [u32; 16], // previous location of SAU entries
    pub version: u32,
    pub epoch: u32,
}

// Corresponds to the ARM vector table, limited to what we need
// see ARMv8m B3.30 and B1.5.3 ARMv7m for the full description
#[repr(C)]
#[derive(Default, AsBytes)]
pub struct ImageVectors {
    pub sp: u32,
    pub entry: u32,
}
