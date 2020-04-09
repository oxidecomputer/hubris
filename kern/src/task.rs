use core::borrow::{Borrow, BorrowMut};
use zerocopy::FromBytes;

use crate::time::Timestamp;
use crate::umem::{USlice, ULease};

/// Internal representation of a task.
pub struct Task {
    /// Current priority of the task.
    pub priority: Priority,
    /// State used to make status and scheduling decisions.
    pub state: TaskState,
    /// Saved machine state of the user program.
    pub save: crate::arch::SavedState,
    /// State for tracking the task's timer.
    pub timer: TimerState,
    /// Generation number of this task's current incarnation. This begins at
    /// zero and gets incremented whenever a task gets rebooted, to try to help
    /// peers notice that they're talking to a new copy that may have lost
    /// state.
    pub generation: Generation,

    /// Static table defining this task's memory regions.
    pub region_table: &'static [MemoryRegion],

    /// Notification status.
    pub notifications: u32,
    /// Notification mask.
    pub notification_mask: u32,
}

impl Task {
    /// Puts this task into a forced fault condition.
    ///
    /// The task will not be scheduled again until the fault is cleared. The
    /// kernel won't clear faults on its own, it must be asked.
    ///
    /// If the task is already faulted, we will retain the information about
    /// what state the task was in *before* it faulted, and *erase* the last
    /// fault. These kinds of double-faults are expected to be super rare.
    ///
    /// Returns a `NextTask` under the assumption that, if you're hitting tasks
    /// with faults, at least one of them is probably the current task; this
    /// makes it harder to forget to request rescheduling. If you're faulting
    /// some other task you can explicitly ignore the result.
    pub fn force_fault(&mut self, fault: FaultInfo) -> NextTask {
        self.state = match self.state {
            TaskState::Healthy(sched) => TaskState::Faulted { original_state: sched, fault },
            TaskState::Faulted { original_state, ..} => {
                // Double fault - fault while faulted
                // Original fault information is lost
                TaskState::Faulted { fault, original_state }
            }
        };
        NextTask::Other
    }

    /// Tests whether this task has read access to `slice` as normal memory.
    /// This is used to validate kernel accessses to the memory.
    pub fn can_read<T>(&self, slice: &USlice<T>) -> bool {
        self.region_table.iter().any(|region| {
            region.covers(slice)
                && region.attributes.contains(RegionAttributes::READ)
                && !region.attributes.contains(RegionAttributes::DEVICE)
        })
    }

    /// Tests whether this task has write access to `slice` as normal memory.
    /// This is used to validate kernel accessses to the memory.
    pub fn can_write<T>(&self, slice: &USlice<T>) -> bool {
        self.region_table.iter().any(|region| {
            region.covers(slice)
                && region.attributes.contains(RegionAttributes::WRITE)
                && !region.attributes.contains(RegionAttributes::DEVICE)
        })
    }

    /// Posts a set of notification bits (which might be empty) to this task.
    /// Returns `true` if an unmasked notification bit is set (whether or not it
    /// is *newly* set) and this task is blocked in receive.
    ///
    /// This would return a `NextTask` but that would require the task to know
    /// its own global ID, which it does not.
    #[must_use]
    pub fn post(&mut self, n: NotificationSet) -> bool {
        self.notifications |= n.0;
        (self.notifications & self.notification_mask != 0)
            && self.state == TaskState::Healthy(SchedState::InRecv(None))
    }
}

/// Static table entry for a task's memory regions.
///
/// Currently, this struct is architecture-neutral, but that means it needs to
/// be converted to be loaded into the memory protection unit on context
/// switch. It may pay to make it architecture-specific and move it out of here.
#[derive(Debug, FromBytes)]
#[repr(C)]
pub struct MemoryRegion {
    pub base: usize,
    pub size: usize,
    pub attributes: RegionAttributes,
}

impl MemoryRegion {
    /// Checks this region's structure. Used early in boot to check region
    /// tables before starting tasks.
    pub fn validate(&self) -> bool {
        // Check that base+size doesn't wrap the address space.
        let highest_base = core::usize::MAX - self.size;
        if self.base > highest_base {
            return false
        }
        // Reject any reserved bits in the attributes word.
        if self.attributes.intersects(RegionAttributes::RESERVED) {
            return false
        }

        true
    }

    /// Tests whether `slice` is fully enclosed by this region.
    pub fn covers<T>(&self, slice: &USlice<T>) -> bool {
        let self_end = self.base.wrapping_add(self.size).wrapping_sub(1);
        let slice_end = slice.last_byte_addr();

        self_end >= slice.base_addr() && slice_end >= self.base
    }
}

bitflags::bitflags! {
    #[derive(FromBytes)]
    #[repr(transparent)]
    pub struct RegionAttributes: u32 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        const EXECUTE = 1 << 2;
        const DEVICE = 1 << 3;
        const RESERVED = !((1 << 4) - 1);
    }
}

/// Indicates priority of a task.
///
/// Priorities are small numbers starting from zero. Numerically lower
/// priorities are more important, so Priority 0 is the most likely to be
/// scheduled, followed by 1, and so forth. (This keeps our logic simpler given
/// that the number of priorities can be reconfigured.)
#[repr(transparent)]
pub struct Priority(u8);

/// Type used to track generation numbers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct Generation(u8);

pub trait ArchState {
    fn stack_pointer(&self) -> u32;

    /// Reads syscall argument register 0.
    fn arg0(&self) -> u32;
    fn arg1(&self) -> u32;
    fn arg2(&self) -> u32;
    fn arg3(&self) -> u32;
    fn arg4(&self) -> u32;
    fn arg5(&self) -> u32;
    fn arg6(&self) -> u32;
    fn arg7(&self) -> u32;

    /// Writes syscall return argument 0.
    fn ret0(&mut self, _: u32);
    fn ret1(&mut self, _: u32);
    fn ret2(&mut self, _: u32);
    fn ret3(&mut self, _: u32);
    fn ret4(&mut self, _: u32);
    fn ret5(&mut self, _: u32);

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for SEND.
    fn as_send_args(&self) -> AsSendArgs<&Self> {
        AsSendArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// return registers for SEND.
    fn as_send_result(&mut self) -> AsSendResult<&mut Self> {
        AsSendResult(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for RECV.
    fn as_recv_args(&self) -> AsRecvArgs<&Self> {
        AsRecvArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// return registers for RECV.
    fn as_recv_result(&mut self) -> AsRecvResult<&mut Self> {
        AsRecvResult(self)
    }
}

/// Reference proxy for send argument registers.
pub struct AsSendArgs<T>(T);

impl<'a, T: ArchState> AsSendArgs<&'a T> {
    /// Extracts the task ID the caller wishes to send to.
    pub fn callee(&self) -> TaskID {
        TaskID((self.0.arg0() >> 16) as u16)
    }

    /// Extracts the operation code the caller is using.
    pub fn operation(&self) -> u16 {
        self.0.arg0() as u16
    }

    /// Extracts the bounds of the caller's message as a `USlice`.
    ///
    /// If the caller passed a slice that overlaps the end of the address space,
    /// returns `Err`.
    pub fn message(&self) -> Result<USlice<u8>, UsageError> {
        USlice::from_raw(self.0.arg1() as usize, self.0.arg2() as usize)
    }

    /// Extracts the bounds of the caller's response buffer as a `USlice`.
    ///
    /// If the caller passed a slice that overlaps the end of the address space,
    /// returns `Err`.
    pub fn response_buffer(&self) -> Result<USlice<u8>, UsageError> {
        USlice::from_raw(self.0.arg3() as usize, self.0.arg4() as usize)
    }

    /// Extracts the bounds of the caller's lease table as a `USlice`.
    ///
    /// If the caller passed a slice that overlaps the end of the address space,
    /// or that is not aligned properly for a lease table, returns `Err`.
    pub fn lease_table(&self) -> Result<USlice<ULease>, UsageError> {
        USlice::from_raw(self.0.arg5() as usize, self.0.arg6() as usize)
    }
}

/// Reference proxy for send result registers.
pub struct AsSendResult<T>(T);

impl<'a, T: ArchState> AsSendResult<&'a mut T> {
    /// Sets the response code and length returned from a send.
    pub fn set_response_and_length(&mut self, resp: u32, len: usize) {
        let r = self.0.borrow_mut();
        r.ret0(resp);
        r.ret1(len as u32);
    }
}

/// Reference proxy for receive argument registers.
pub struct AsRecvArgs<T>(T);

impl<'a, T: ArchState> AsRecvArgs<&'a T> {
    /// Gets the caller's receive destination buffer.
    ///
    /// If the callee provided a bogus destination slice, this will return
    /// `Err`.
    pub fn buffer(&self) -> Result<USlice<u8>, UsageError> {
        let b = self.0.borrow();
        USlice::from_raw(b.arg0() as usize, b.arg1() as usize)
    }
}

/// Reference proxy for receive return registers.
pub struct AsRecvResult<T>(T);

impl<'a, T: ArchState> AsRecvResult<&'a mut T> {
    /// Sets the sender of a message.
    pub fn set_sender(&mut self, sender: TaskID) {
        self.0.borrow_mut().ret0(u32::from(sender.0));
    }

    /// Sets the operation code associated with a message.
    pub fn set_operation(&mut self, operation: u16) {
        self.0.borrow_mut().ret1(u32::from(operation));
    }

    /// Sets the length of a received message.
    pub fn set_message_len(&mut self, length: usize) {
        self.0.borrow_mut().ret2(length as u32);
    }

    /// Sets the size of the response buffer at the caller.
    pub fn set_response_capacity(&mut self, length: usize) {
        self.0.borrow_mut().ret3(length as u32);
    }

    /// Sets the number of leases provided by the caller.
    pub fn set_lease_count(&mut self, count: usize) {
        self.0.borrow_mut().ret4(count as u32);
    }
}

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

/// Scheduler parameters for a healthy task.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SchedState {
    /// This task is ignored for scheduling purposes.
    Stopped,
    /// This task could be scheduled on the CPU.
    Runnable,
    /// This task is blocked waiting to deliver a message to the given task.
    SendingTo(usize),
    /// This task is blocked waiting for a reply from the given task.
    AwaitingReplyFrom(usize),
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
}

impl From<UsageError> for FaultInfo {
    fn from(e: UsageError) -> Self {
        Self::SyscallUsage(e)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// A program specified a slice as a syscall argument, but the slice is
    /// patently invalid: it is either unaligned for its type, or it is
    /// expressed such that it would wrap around the end of the address space.
    /// Neither of these conditions is ever legal, so this represents a
    /// malfunction in the caller.
    InvalidSlice,
    /// A program named a task ID that will never be valid, as it's out of
    /// range.
    TaskOutOfRange,
}

/// Origin of a fault.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FaultSource {
    /// User code did something that was intercepted by the processor.
    User,
    /// User code asked the kernel to do something bad on its behalf.
    Kernel,
}

/// Type used at the syscall boundary to name tasks.
///
/// A `TaskID` is a combination of a task index (statically fixed) and a
/// generation number. The generation changes each time the task is rebooted, to
/// detect discontinuities in IPC conversations.
///
/// The split between the two is given by `TaskID::IDX_BITS`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct TaskID(u16);

impl TaskID {
    /// Number of bits in the ID portion of a `TaskID`. The remaining bits are
    /// generation.
    pub const IDX_BITS: u32 = 10;
    /// Mask derived from `IDX_BITS` for extracting the task index.
    pub const IDX_MASK: u16 = (1 << Self::IDX_BITS) - 1;

    /// Fabricates a `TaskID` for the given index and generation.
    pub fn from_index_and_gen(index: usize, gen: Generation) -> Self {
        Self((gen.0 as u16) << Self::IDX_BITS | (index as u16 & Self::IDX_MASK))
    }

    /// Extracts the index part of this ID.
    pub fn index(&self) -> usize {
        usize::from(self.0 & Self::IDX_MASK)
    }

    /// Extracts the generation part of this ID.
    pub fn generation(&self) -> Generation {
        Generation((self.0 >> Self::IDX_BITS) as u8)
    }
}

/// State for a task timer.
///
/// Task timers are used to multiplex the hardware timer.
pub struct TimerState {
    /// Deadline, in kernel time, at which this timer should fire. If `None`,
    /// the timer is disabled.
    deadline: Option<Timestamp>,
    /// Set of notification bits to post to the owning task when this timer
    /// fires.
    to_post: NotificationSet,
}

/// Collection of bits that may be posted to a task's notification word.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct NotificationSet(u32);

/// Return value for operations that can have scheduling implications. This is
/// marked `must_use` because forgetting to actually update the scheduler after
/// performing an operation that requires it would be Bad.
#[must_use]
pub enum NextTask {
    /// It's fine to keep running whatever task we were just running.
    Same,
    /// We need to switch tasks, but this routine has not concluded which one
    /// should now run. The scheduler needs to figure it out.
    Other,
    /// We need to switch tasks, and we already know which one should run next.
    /// This is an optimization available in certain IPC cases.
    Specific(usize),
}

impl NextTask {
    pub fn combine(self, other: Self) -> Self {
        use NextTask::*;  // shorthand for patterns

        match (self, other) {
            // If we have two specific suggestions, and they disagree, we punt
            // to the scheduler to figure out the best option.
            (Specific(x), Specific(y)) if x != y => Other,
            // Otherwise, if either suggestion is a specific switch, take it.
            // This covers: matching specifics; specific+unspecific;
            // specific+same.
            (Specific(x), _) | (_, Specific(x)) => Specific(x),
            // Otherwise, if either suggestion says switch, switch.
            (Other, _) | (_, Other) => Other,
            // All we have left is...
            (Same, Same) => Same,
        }
    }
}

/// Processes all enabled timers in the task table, posting notifications for
/// any that have expired by `current_time` (and disabling them atomically).
pub fn process_timers(tasks: &mut [Task], current_time: Timestamp) -> NextTask {
    let mut sched_hint = NextTask::Same;
    for (index, task) in tasks.iter_mut().enumerate() {
        if let Some(deadline) = task.timer.deadline {
            if deadline <= current_time {
                task.timer.deadline = None;
                let task_hint = if task.post(task.timer.to_post) {
                    NextTask::Specific(index)
                } else {
                    NextTask::Same
                };
                sched_hint = sched_hint.combine(task_hint)
            }
        }
    }
    sched_hint
}

/// Checks a user-provided `TaskID` for validity against `table`.
///
/// On success, returns an index that can be used to dereference `table` without
/// panicking.
///
/// On failure, indicates the condition by `TaskIDError`.
pub fn check_task_id_against_table(
    table: &[Task],
    id: TaskID,
) -> Result<usize, TaskIDError> {
    if id.index() >= table.len() {
        return Err(TaskIDError::OutOfRange);
    }

    // Check for dead task ID.
    if table[id.index()].generation != id.generation() {
        return Err(TaskIDError::Stale);
    }

    return Ok(id.index())
}

/// Problems we might discover about `TaskID` values.
#[must_use]
pub enum TaskIDError {
    /// The provided task ID addresses a task that will never exist. This is a
    /// malfunction of the sender and needs to cause a fault.
    OutOfRange,
    /// The task ID describes a previous generation of this task, suggesting
    /// that the peer has died since last contacted. This is expressed to the
    /// caller as an error code.
    Stale,
}
