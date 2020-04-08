//! Hubris kernel model.
//!
//! This code is intended to lay out the design concepts for the Hubris kernel
//! implementation and make some points about algorithm implementation. It may
//! evolve to become the actual kernel, or it may not.
//!
//! Currently, this is intended to be portable to both ARM and x86, for testing
//! and simulation purposes.
//!
//! # Algorithm Naivety Principles
//!
//! This implementation uses *really naive algorithms*. This is deliberate. The
//! intent is:
//!
//! 1. To use safe Rust for as much as possible.
//! 2. To use easily understood and debugged algorithms.
//! 3. To revisit these decisions if they become performance problems.
//!
//! Assumptions enabling our naivete:
//!
//! - The total number of tasks is fixed (in a given build) and small. Say, less
//!   than 200.
//! - We are not attempting to achieve predictably low worst-case execution
//!   bounds or any realtime nonsense like that.

use core::borrow::{Borrow, BorrowMut};
use core::marker::PhantomData;

/// Response code returned by the kernel to signal that an IPC failed because
/// the peer died.
pub const DEAD: u32 = !0;

/// Internal representation of a task.
pub struct Task {
    /// Current priority of the task.
    priority: Priority,
    /// State used to make status and scheduling decisions.
    state: TaskState,
    /// Saved machine state of the user program.
    save: SavedState,
    /// State for tracking the task's timer.
    timer: TimerState,
    /// Generation number of this task's current incarnation. This begins at
    /// zero and gets incremented whenever a task gets rebooted, to try to help
    /// peers notice that they're talking to a new copy that may have lost
    /// state.
    generation: Generation,
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
struct SavedState {
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
struct AsSendArgs<T>(T);

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
    pub fn message(&self) -> USlice<u8> {
        let b = self.0.borrow();
        USlice::from_raw(b.arg1() as usize, b.arg2() as usize)
    }

    /// Extracts the bounds of the caller's response buffer as a `USlice`.
    pub fn response_buffer(&self) -> USlice<u8> {
        let b = self.0.borrow();
        USlice::from_raw(b.arg3() as usize, b.arg4() as usize)
    }

    /// Extracts the bounds of the caller's lease table as a `USlice`.
    pub fn lease_table(&self) -> USlice<ULease> {
        let b = self.0.borrow();
        USlice::from_raw(b.arg5() as usize, b.arg6() as usize)
    }
}

/// Reference proxy for send result registers.
struct AsSendResult<T>(T);

impl<T: BorrowMut<SavedState>> AsSendResult<T> {
    /// Sets the response code and length returned from a send.
    pub fn set_response_and_length(&mut self, resp: u32, len: usize) {
        let r = self.0.borrow_mut();
        r.ret0(resp);
        r.ret1(len as u32);
    }
}

/// Reference proxy for receive argument registers.
struct AsRecvArgs<T>(T);

impl<T: Borrow<SavedState>> AsRecvArgs<T> {
    /// Gets the caller's receive destination buffer.
    pub fn buffer(&self) -> USlice<u8> {
        let b = self.0.borrow();
        USlice::from_raw(b.arg0() as usize, b.arg1() as usize)
    }
}

/// Reference proxy for receive return registers.
struct AsRecvResult<T>(T);

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

/// A (user, untrusted, unprivileged) slice.
///
/// A `USlice` references memory from a task, outside the kernel. The slice is
/// alleged to contain values of type `T`, but is not guaranteed to be correctly
/// aligned, etc.
///
/// The existence of a `USlice` only tells you one thing: that a task has
/// asserted that it has access to a range of memory addresses. It does not
/// *prove* that the task has this access, that it is aligned, that is is
/// correctly initialized, etc. The result must be used carefully.
///
/// Currently, the same `USlice` type is used for both readable and read-write
/// task memory. They are distinguished only by context. This might prove to be
/// annoying.
pub struct USlice<T> {
    /// Base address of the slice.
    base_address: usize,
    /// Number of `T` elements in the slice.
    length: usize,
    /// since we don't actually use T...
    _marker: PhantomData<*mut [T]>,
}

impl<T> USlice<T> {
    pub fn from_raw(base_address: usize, length: usize) -> Self {
        Self { base_address, length, _marker: PhantomData }
    }
}

/// Structure describing a lease in task memory. This is an ABI commitment.
///
/// At SEND, the task gives us the base and length of a section of memory that
/// it *claims* contains structs of this type.
#[derive(Debug)]
#[repr(C)]
pub struct ULease {
    /// Lease attributes.
    ///
    /// Currently, bit 0 indicates readable memory, and bit 1 indicates writable
    /// memory. All other bits are currently undefined and should be zero.
    pub attributes: u32,
    /// Base address of leased memory. This is equivalent to the base address
    /// field in `USlice`, but isn't represented as a `USlice` because we leave
    /// the internal memory representation of `USlice` out of the ABI.
    pub base_address: usize,
    /// Length of leased memory, in bytes.
    pub length: usize,
}

/// Extracts the base/bound part of a `ULease` as a `USlice` of bytes.
impl<'a> From<&'a ULease> for USlice<u8> {
    fn from(lease: &'a ULease) -> Self {
        Self {
            base_address: lease.base_address,
            length: lease.length,
            _marker: PhantomData,
        }
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
    }
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

/// In-kernel timestamp representation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(transparent)]
pub struct Timestamp(u64);

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

/// Implementation of the SEND IPC primitive.
pub fn send(tasks: &mut [Task], caller: usize) -> NextTask {
    // Extract callee.
    let callee = tasks[caller].save.as_send_args().callee();
    // Check IPC filter - TODO
    // Check for dead task ID.
    if tasks[callee.index()].generation == callee.generation() {
        let callee = callee.index();
        // Check for ready peer.
        match &tasks[callee].state {
            TaskState::Healthy(SchedState::Receiving(from)) if from.unwrap_or(caller) == caller => {
                // We can skip a step.
                tasks[caller].state = TaskState::Healthy(SchedState::AwaitingReplyFrom(callee));
                match deliver(tasks, caller, callee) {
                    Ok(_) => {
                        tasks[callee].state = TaskState::Healthy(SchedState::Runnable);
                        // Propose switching directly to the unblocked callee.
                        NextTask::Specific(callee)
                    }
                    Err(switch) => {
                        // Delivery failed and returned hints about the next
                        // context switch.
                        switch
                    }
                }
            },
            _ => {
                // Caller needs to block sending, callee is either busy or
                // faulted.
                tasks[caller].state = TaskState::Healthy(SchedState::SendingTo(callee));
                // We don't know what the best task to run now would be, but
                // we're pretty darn sure it isn't the caller.
                return NextTask::Other
            }
        }
    } else {
        // Inform caller by resuming it with an error response code.
        resume_sender_with_error(&mut tasks[caller]);
        NextTask::Same
    }
}

/// Transfers a message from caller's context into callee's.
///
/// Preconditions:
///
/// - Caller is sending -- either blocked in state `SendingTo`, or in the
///   process of transitioning from `Runnable` to `AwaitingReplyFrom`.
/// - Callee is receiving -- either blocked in `Receiving` or in `Runnable`
///   executing a receive system call.
///
/// Deliver may fail due to a fault in either or both task. In that case, it
/// will stuff the precise fault into the task's scheduling state and return
/// `Err` indicating that a task switch is required, under the assumption that
/// at least one of the tasks involved in the `deliver` call was running. (Which
/// is a good assumption in general.)
///
/// On success, returns `Ok(())` and any task-switching is the caller's
/// responsibility.
fn deliver(tasks: &mut [Task], caller: usize, callee: usize) -> Result<(), NextTask> {
    // Collect information on the send from the caller. This information is all
    // stored in infallibly-readable areas.
    let send_args = tasks[caller].save.as_send_args();
    let op = send_args.operation();
    let caller_id = TaskID::from_index_and_gen(caller, tasks[caller].generation);
    let src_slice = send_args.message();
    let response_capacity = send_args.response_buffer().length;
    let lease_count = send_args.lease_table().length;
    drop(send_args);

    // Collect information about the callee's receive buffer. This, too, is
    // somewhere we can read infallibly.
    let recv_args = tasks[callee].save.as_recv_args();
    let dest_slice = recv_args.buffer();
    drop(recv_args);

    // Okay, now we do things that can fail.
    match safe_copy(&tasks[caller], src_slice, &tasks[callee], dest_slice) {
        Err(CopyError { src_fault, dest_fault }) => {
            // One task or the other lied about their memory layout. Find the
            // culprit(s) and switch them into faulted state.
            let src_switch = if let Some(addr) = src_fault {
                tasks[caller].force_fault(FaultInfo::MemoryAccess {
                    address: Some(addr),
                    source: FaultSource::Kernel,
                })
            } else {
                NextTask::Same
            };
            let dest_switch = if let Some(addr) = dest_fault {
                tasks[callee].force_fault(FaultInfo::MemoryAccess {
                    address: Some(addr),
                    source: FaultSource::Kernel,
                })
            } else {
                NextTask::Same
            };
            Err(src_switch.combine(dest_switch))
        },
        Ok(amount_copied) => {
            // We were able to transfer the message.
            let mut rr = tasks[callee].save.as_recv_result();
            rr.set_sender(caller_id);
            rr.set_operation(op);
            rr.set_message_len(amount_copied);
            rr.set_response_capacity(response_capacity);
            rr.set_lease_count(lease_count);
            drop(rr);

            tasks[caller].state = TaskState::Healthy(SchedState::AwaitingReplyFrom(callee));
            tasks[callee].state = TaskState::Healthy(SchedState::Runnable);
            // We don't have an opinion about the newly runnable task, nor do we
            // have enough information to insist that a switch must happen.
            Ok(())
        },
    }
}

/// Updates `task`'s registers to show that the send syscall failed.
///
/// This is factored out because I'm betting we're going to want it in a bunch
/// of places. That might prove wrong.
fn resume_sender_with_error(task: &mut Task) {
    let mut r = task.save.as_send_result();
    r.set_response_and_length(DEAD, 0);
}

/// Copies bytes from task `from` in region `from_slice` into task `to` at
/// region `to_slice`, checking memory access before doing so.
///
/// The actual number of bytes copied will be `min(from_slice.length,
/// to_slice.length)`, and will be returned.
///
/// If `from_slice` or `to_slice` refers to memory the task can't read or write
/// (respectively), no bytes are copied, and this returns a `CopyError`
/// indicating which task(s) messed this up.
fn safe_copy(
    _from: &Task,
    _from_slice: USlice<u8>,
    _to: &Task,
    _to_slice: USlice<u8>,
) -> Result<usize, CopyError> {
    unimplemented!()
}

/// Failure information returned from `safe_copy`.
///
/// The faulting addresses returned in this struct provide *examples* of an
/// illegal address. The precise choice of faulting address within a bad slice
/// is left undefined.
struct CopyError {
    /// Address where source would have faulted.
    src_fault: Option<usize>,
    /// Address where dest would have faulted.
    dest_fault: Option<usize>,
}
