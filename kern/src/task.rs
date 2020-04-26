use abi::Priority;

use crate::app::{RegionAttributes, RegionDesc, RegionDescExt, TaskDesc};
use crate::err::UserError;
use crate::time::Timestamp;
use crate::umem::{ULease, USlice};

/// Internal representation of a task.
#[repr(C)] // so location of SavedState is predictable
#[derive(Debug)]
pub struct Task {
    /// Saved machine state of the user program.
    pub save: crate::arch::SavedState,
    // NOTE: it is critical that the above field appear first!
    /// Current priority of the task.
    pub priority: Priority,
    /// State used to make status and scheduling decisions.
    pub state: TaskState,
    /// State for tracking the task's timer.
    pub timer: TimerState,
    /// Generation number of this task's current incarnation. This begins at
    /// zero and gets incremented whenever a task gets rebooted, to try to help
    /// peers notice that they're talking to a new copy that may have lost
    /// state.
    pub generation: Generation,

    /// Static table defining this task's memory regions.
    pub region_table: &'static [&'static RegionDesc],

    /// Notification status.
    pub notifications: u32,
    /// Notification mask.
    pub notification_mask: u32,

    /// Pointer to the ROM descriptor used to create this task, so it can be
    /// restarted.
    pub descriptor: &'static TaskDesc,
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
            TaskState::Healthy(sched) => TaskState::Faulted {
                original_state: sched,
                fault,
            },
            TaskState::Faulted { original_state, .. } => {
                // Double fault - fault while faulted
                // Original fault information is lost
                TaskState::Faulted {
                    fault,
                    original_state,
                }
            }
        };
        NextTask::Other
    }

    /// Tests whether this task has read access to `slice` as normal memory.
    /// This is used to validate kernel accessses to the memory.
    pub fn can_read<T>(&self, slice: &USlice<T>) -> bool {
        if slice.is_empty() {
            return true;
        }
        self.region_table.iter().any(|region| {
            region.covers(slice)
                && region.attributes.contains(RegionAttributes::READ)
                && !region.attributes.contains(RegionAttributes::DEVICE)
        })
    }

    /// Tests whether this task has write access to `slice` as normal memory.
    /// This is used to validate kernel accessses to the memory.
    pub fn can_write<T>(&self, slice: &USlice<T>) -> bool {
        if slice.is_empty() {
            return true;
        }
        self.region_table.iter().any(|region| {
            region.covers(slice)
                && region.attributes.contains(RegionAttributes::WRITE)
                && !region.attributes.contains(RegionAttributes::DEVICE)
        })
    }

    /// Posts a set of notification bits (which might be empty) to this task. If
    /// the task is blocked in receive, and any of the bits match the
    /// notification mask, unblocks the task and returns `true` (indicating that
    /// a context switch may be necessary). If no context switch is required,
    /// returns `false`.
    ///
    /// This would return a `NextTask` but that would require the task to know
    /// its own global ID, which it does not.
    #[must_use]
    pub fn post(&mut self, n: NotificationSet) -> bool {
        self.notifications |= n.0;
        let firing = self.notifications & self.notification_mask;
        if firing != 0 {
            if self.state == TaskState::Healthy(SchedState::InRecv(None)) {
                self.save.set_recv_result(TaskID::KERNEL, firing, 0, 0, 0);
                self.state = TaskState::Healthy(SchedState::Runnable);
                self.acknowledge_notifications();
                return true;
            }
        }
        false
    }

    /// Updates the task's notification mask.
    ///
    /// This may cause notifications that were previously posted to fire. If
    /// they fire, they will be returned to you in a `Some` but will not be
    /// acknowledged (cleared). If you are updating the notification mask as a
    /// side effect of receive, you should deliver the notifications; if this
    /// is happening for some other reason you might leave the task with
    /// notifications pending.
    #[must_use]
    pub fn update_mask(&mut self, m: u32) -> Option<u32> {
        self.notification_mask = m;
        let firing = self.notifications & self.notification_mask;
        if firing != 0 {
            Some(firing)
        } else {
            None
        }
    }

    /// Clears notification bits that are set in `bits`. Use this to signal that
    /// some notifications were delivered, otherwise they'll keep firing.
    pub fn acknowledge_notifications(&mut self) {
        self.notifications &= !self.notification_mask;
    }

    /// Checks if this task is in a potentially schedulable state.
    pub fn is_runnable(&self) -> bool {
        self.state == TaskState::Healthy(SchedState::Runnable)
    }

    pub fn set_timer(
        &mut self,
        deadline: Option<Timestamp>,
        notifications: NotificationSet,
    ) {
        self.timer.deadline = deadline;
        self.timer.to_post = notifications;
    }
}

/// Type used to track generation numbers.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
#[repr(transparent)]
pub struct Generation(u8);

pub trait ArchState: Default {
    fn stack_pointer(&self) -> u32;

    /// Reads syscall argument register 0.
    fn arg0(&self) -> u32;
    fn arg1(&self) -> u32;
    fn arg2(&self) -> u32;
    fn arg3(&self) -> u32;
    fn arg4(&self) -> u32;
    fn arg5(&self) -> u32;
    fn arg6(&self) -> u32;

    fn syscall_descriptor(&self) -> u32;

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
    /// arguments for RECV.
    fn as_recv_args(&self) -> AsRecvArgs<&Self> {
        AsRecvArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for REPLY.
    fn as_reply_args(&self) -> AsReplyArgs<&Self> {
        AsReplyArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for TIMER.
    fn as_timer_args(&self) -> AsTimerArgs<&Self> {
        AsTimerArgs(self)
    }

    /// Sets a recoverable error code using the generic ABI.
    fn set_error_response(&mut self, resp: u32) {
        self.ret0(resp);
        self.ret1(0);
    }

    /// Sets the response code and length returned from a SEND.
    fn set_send_response_and_length(&mut self, resp: u32, len: usize) {
        self.ret0(resp);
        self.ret1(len as u32);
    }

    /// Sets the results returned from a RECV.
    fn set_recv_result(
        &mut self,
        sender: TaskID,
        operation: u32,
        length: usize,
        response_capacity: usize,
        lease_count: usize,
    ) {
        self.ret0(0);  // currently reserved
        self.ret1(u32::from(sender.0));
        self.ret2(operation);
        self.ret3(length as u32);
        self.ret4(response_capacity as u32);
        self.ret5(lease_count as u32);
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

/// Reference proxy for receive argument registers.
pub struct AsRecvArgs<T>(T);

impl<'a, T: ArchState> AsRecvArgs<&'a T> {
    /// Gets the caller's receive destination buffer.
    ///
    /// If the callee provided a bogus destination slice, this will return
    /// `Err`.
    pub fn buffer(&self) -> Result<USlice<u8>, UsageError> {
        USlice::from_raw(self.0.arg0() as usize, self.0.arg1() as usize)
    }

    /// Gets the caller's notification mask.
    pub fn notification_mask(&self) -> u32 {
        self.0.arg2()
    }
}

/// Reference proxy for reply argument registers.
pub struct AsReplyArgs<T>(T);

impl<'a, T: ArchState> AsReplyArgs<&'a T> {
    /// Extracts the task ID the caller wishes to reply to.
    pub fn callee(&self) -> TaskID {
        TaskID(self.0.arg0() as u16)
    }

    /// Extracts the response code the caller is using.
    pub fn response_code(&self) -> u32 {
        self.0.arg1()
    }

    /// Extracts the bounds of the caller's reply buffer as a `USlice`.
    ///
    /// If the caller passed a slice that overlaps the end of the address space,
    /// returns `Err`.
    pub fn message(&self) -> Result<USlice<u8>, UsageError> {
        USlice::from_raw(self.0.arg2() as usize, self.0.arg3() as usize)
    }
}

/// Reference proxy for TIMER argument registers.
pub struct AsTimerArgs<T>(T);

impl<'a, T: ArchState> AsTimerArgs<&'a T> {
    /// Extracts the deadline.
    pub fn deadline(&self) -> Option<Timestamp> {
        if self.0.arg0() != 0 {
            Some(Timestamp::from(
                u64::from(self.0.arg2()) << 32 | u64::from(self.0.arg1()),
            ))
        } else {
            None
        }
    }

    /// Extracts the notification set.
    pub fn notification(&self) -> NotificationSet {
        NotificationSet(self.0.arg3())
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
}

impl From<UsageError> for FaultInfo {
    fn from(e: UsageError) -> Self {
        Self::SyscallUsage(e)
    }
}

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
    pub const KERNEL: Self = TaskID(core::u16::MAX);

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
#[derive(Debug, Default)]
pub struct TimerState {
    /// Deadline, in kernel time, at which this timer should fire. If `None`,
    /// the timer is disabled.
    deadline: Option<Timestamp>,
    /// Set of notification bits to post to the owning task when this timer
    /// fires.
    to_post: NotificationSet,
}

/// Collection of bits that may be posted to a task's notification word.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
#[repr(transparent)]
pub struct NotificationSet(u32);

/// Return value for operations that can have scheduling implications. This is
/// marked `must_use` because forgetting to actually update the scheduler after
/// performing an operation that requires it would be Bad.
#[derive(Clone, Debug, Eq, PartialEq)]
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
        use NextTask::*; // shorthand for patterns

        match (self, other) {
            // If both agree, our job is easy.
            (x, y) if x == y => x,
            // Specific task recommendations that *don't* agree get downgraded
            // to Other.
            (Specific(_), Specific(_)) => Other,
            // If only *one* is specific, it wins.
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
/// On failure, indicates the condition by `UserError`.
pub fn check_task_id_against_table(
    table: &[Task],
    id: TaskID,
) -> Result<usize, UserError> {
    if id.index() >= table.len() {
        return Err(FaultInfo::SyscallUsage(UsageError::TaskOutOfRange).into());
    }

    // Check for dead task ID.
    if table[id.index()].generation != id.generation() {
        return Err(UserError::Recoverable(abi::DEAD));
    }

    return Ok(id.index());
}

/// Selects a new task to run after `previous`. Tries to be fair, kind of.
///
/// If no tasks are runnable, the kernel panics.
pub fn select(previous: usize, tasks: &[Task]) -> usize {
    priority_scan(previous, tasks, |t| t.is_runnable())
        .expect("no tasks runnable")
}

/// Scans `tasks` for the next task, after `previous`, that satisfies `pred`. If
/// more than one task satisfies `pred`, returns the most important one. If
/// multiple tasks with the same priority satisfy `pred`, prefers the first one
/// in order after `previous`, mod `tasks.len()`.
///
/// Whew.
///
/// This is generally the right way to search a task table, and is used to
/// implement (among other bits) the scheduler.
pub fn priority_scan(
    previous: usize,
    tasks: &[Task],
    pred: impl Fn(&Task) -> bool,
) -> Option<usize> {
    let search_order = (previous + 1..tasks.len()).chain(0..previous + 1);
    let mut choice = None;
    for i in search_order {
        if !pred(&tasks[i]) {
            continue;
        }

        if let Some((_, prio)) = choice {
            if !tasks[i].priority.is_more_important_than(prio) {
                continue;
            }
        }

        choice = Some((i, tasks[i].priority));
    }

    choice.map(|(idx, _)| idx)
}
