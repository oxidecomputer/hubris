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
    pub save: SavedState,
    /// State for tracking the task's timer.
    pub timer: TimerState,
    /// Generation number of this task's current incarnation. This begins at
    /// zero and gets incremented whenever a task gets rebooted, to try to help
    /// peers notice that they're talking to a new copy that may have lost
    /// state.
    pub generation: Generation,

    /// Static table defining this task's memory regions.
    pub region_table: &'static [MemoryRegion],
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

/// The portion of the task's machine state that is not automatically saved by
/// hardware onto the stack.
///
/// On ARMv7-M this will be small. On RISC-V this will be large. On a simulator
/// this will be weird.
///
/// One of this kernel's odd design constraints is that, other than copying
/// messages, it will *only* read or write args and results to this struct. It
/// never messes with the user stack except as required at context switch.
pub struct SavedState {
}

impl SavedState {
    /// Reads syscall argument register 0.
    fn arg0(&self) -> u32 {
        unimplemented!()
    }
    fn arg1(&self) -> u32 {
        unimplemented!()
    }
    fn arg2(&self) -> u32 {
        unimplemented!()
    }
    fn arg3(&self) -> u32 {
        unimplemented!()
    }
    fn arg4(&self) -> u32 {
        unimplemented!()
    }
    fn arg5(&self) -> u32 {
        unimplemented!()
    }
    fn arg6(&self) -> u32 {
        unimplemented!()
    }
    fn arg7(&self) -> u32 {
        unimplemented!()
    }

    /// Writes syscall return argument 0.
    fn ret0(&mut self, _: u32) {
        unimplemented!()
    }
    fn ret1(&mut self, _: u32) {
        unimplemented!()
    }
    fn ret2(&mut self, _: u32) {
        unimplemented!()
    }
    fn ret3(&mut self, _: u32) {
        unimplemented!()
    }
    fn ret4(&mut self, _: u32) {
        unimplemented!()
    }
    fn ret5(&mut self, _: u32) {
        unimplemented!()
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for SEND.
    pub fn as_send_args(&self) -> AsSendArgs<&Self> {
        AsSendArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// return registers for SEND.
    pub fn as_send_result(&mut self) -> AsSendResult<&mut Self> {
        AsSendResult(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for RECV.
    pub fn as_recv_args(&self) -> AsRecvArgs<&Self> {
        AsRecvArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// return registers for RECV.
    pub fn as_recv_result(&mut self) -> AsRecvResult<&mut Self> {
        AsRecvResult(self)
    }
}

/// Reference proxy for send argument registers.
pub struct AsSendArgs<T>(T);

impl<T: Borrow<SavedState>> AsSendArgs<T> {
    /// Extracts the task ID the caller wishes to send to.
    pub fn callee(&self) -> TaskID {
        TaskID((self.0.borrow().arg0() >> 16) as u16)
    }

    /// Extracts the operation code the caller is using.
    pub fn operation(&self) -> u16 {
        self.0.borrow().arg0() as u16
    }

    /// Extracts the bounds of the caller's message as a `USlice`.
    ///
    /// If the caller passed a slice that overlaps the end of the address space,
    /// returns `None`.
    pub fn message(&self) -> Option<USlice<u8>> {
        let b = self.0.borrow();
        USlice::from_raw(b.arg1() as usize, b.arg2() as usize)
    }

    /// Extracts the bounds of the caller's response buffer as a `USlice`.
    ///
    /// If the caller passed a slice that overlaps the end of the address space,
    /// returns `None`.
    pub fn response_buffer(&self) -> Option<USlice<u8>> {
        let b = self.0.borrow();
        USlice::from_raw(b.arg3() as usize, b.arg4() as usize)
    }

    /// Extracts the bounds of the caller's lease table as a `USlice`.
    ///
    /// If the caller passed a slice that overlaps the end of the address space,
    /// or that is not aligned properly for a lease table, returns `None`.
    pub fn lease_table(&self) -> Option<USlice<ULease>> {
        let b = self.0.borrow();
        USlice::from_raw(b.arg5() as usize, b.arg6() as usize)
    }
}

/// Reference proxy for send result registers.
pub struct AsSendResult<T>(T);

impl<T: BorrowMut<SavedState>> AsSendResult<T> {
    /// Sets the response code and length returned from a send.
    pub fn set_response_and_length(&mut self, resp: u32, len: usize) {
        let r = self.0.borrow_mut();
        r.ret0(resp);
        r.ret1(len as u32);
    }
}

/// Reference proxy for receive argument registers.
pub struct AsRecvArgs<T>(T);

impl<T: Borrow<SavedState>> AsRecvArgs<T> {
    /// Gets the caller's receive destination buffer.
    ///
    /// If the callee provided a bogus destination slice, this will return
    /// `None`.
    pub fn buffer(&self) -> Option<USlice<u8>> {
        let b = self.0.borrow();
        USlice::from_raw(b.arg0() as usize, b.arg1() as usize)
    }
}

/// Reference proxy for receive return registers.
pub struct AsRecvResult<T>(T);

impl<T: BorrowMut<SavedState>> AsRecvResult<T> {
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
#[derive(Copy, Clone, Debug)]
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
#[derive(Copy, Clone, Debug)]
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
    Receiving(Option<usize>),
}

/// A record describing a fault taken by a task.
#[derive(Copy, Clone, Debug)]
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
    BadArgs,
}

/// Origin of a fault.
#[derive(Copy, Clone, Debug)]
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

/// Produces an iterator over the subset of `tasks` whose timers are firing at
/// `current_time`.
pub fn firing_timers(tasks: &[Task], current_time: Timestamp) -> impl Iterator<Item = &Task> {
    tasks.iter()
        .filter(move |t| t.timer.deadline.map(|dl| dl <= current_time).unwrap_or(false))
}


