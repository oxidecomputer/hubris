// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Implementation of tasks.

use core::convert::TryFrom;
use core::sync::atomic::{AtomicU32, Ordering};

use abi::{
    FaultInfo, FaultSource, Generation, Priority, ReplyFaultReason, SchedState,
    TaskId, TaskState, UsageError,
};
use zerocopy::FromBytes;

use crate::app::{
    RegionAttributes, RegionDesc, RegionDescExt, TaskDesc, TaskFlags,
};
use crate::err::UserError;
use crate::time::Timestamp;
use crate::umem::{ULease, USlice};

/// This global holds the fault notification that will be sent to the supervisor
/// of another task faults. It gets configured at application startup by a call
/// to `set_fault_notification` and then remains untouched.
#[no_mangle]
static FAULT_NOTIFICATION: AtomicU32 = AtomicU32::new(0);

/// Sets the notification bits that will be posted to the supervisor if another
/// task faults. This is normally invoked only once during startup (though it is
/// not technically unsafe to do other things with it).
pub fn set_fault_notification(mask: u32) {
    FAULT_NOTIFICATION.store(mask, Ordering::Relaxed);
}

/// Internal representation of a task.
///
/// The fields of this struct are private to this module so that we can maintain
/// some task invariants. These mostly have to do with ensuring that task
/// interactions remain consistent across state changes -- for example, setting
/// a task to RECV should process another task trying to SEND, if one exists.
#[repr(C)] // so location of SavedState is predictable
#[derive(Debug)]
pub struct Task {
    /// Saved machine state of the user program.
    save: crate::arch::SavedState,
    // NOTE: it is critical that the above field appear first!
    /// Current priority of the task.
    priority: Priority,
    /// State used to make status and scheduling decisions.
    state: TaskState,
    /// State for tracking the task's timer.
    timer: TimerState,
    /// Restart count for this task. We increment this whenever we reinitialize
    /// the task. The low bits of this become the task's generation number.
    generation: u32,

    /// Static table defining this task's memory regions.
    region_table: &'static [&'static RegionDesc],

    /// Notification status.
    notifications: u32,

    /// Pointer to the ROM descriptor used to create this task, so it can be
    /// restarted.
    descriptor: &'static TaskDesc,
}

impl Task {
    /// Creates a `Task` in its initial state, filling in fields from
    /// `descriptor`.
    pub fn from_descriptor(
        descriptor: &'static TaskDesc,
        region_table: &'static [&'static RegionDesc],
    ) -> Self {
        Task {
            priority: abi::Priority(descriptor.priority as u8),
            state: if descriptor.flags.contains(TaskFlags::START_AT_BOOT) {
                TaskState::Healthy(SchedState::Runnable)
            } else {
                TaskState::default()
            },

            descriptor,
            region_table,

            generation: 0,
            notifications: 0,
            save: crate::arch::SavedState::default(),
            timer: crate::task::TimerState::default(),
        }
    }

    /// Tests whether this task has read access to `slice` as normal memory.
    /// This is used to validate kernel accessses to the memory.
    ///
    /// This is shorthand for `can_access(slice, RegionAttributes::READ)`.
    ///
    /// This function is `must_use` because calling it without checking its
    /// return value is incredibly suspicious.
    #[must_use]
    fn can_read<T>(&self, slice: &USlice<T>) -> bool {
        self.can_access(slice, RegionAttributes::READ)
    }

    /// Obtains access to the memory backing `slice` as a Rust slice, assuming
    /// that the task `self` can access it for read. This is used to access task
    /// memory from the kernel in validated form.
    pub fn try_read<'a, T>(
        &'a self,
        slice: &'a USlice<T>,
    ) -> Result<&'a [T], FaultInfo>
    where
        T: FromBytes,
    {
        if self.can_read(slice) {
            // Safety: assume_readable requires us to have validated that the
            // slice refers to normal task memory, which we did on the previous
            // line.
            unsafe { Ok(slice.assume_readable()) }
        } else {
            Err(FaultInfo::MemoryAccess {
                address: Some(slice.base_addr() as u32),
                source: FaultSource::Kernel,
            })
        }
    }

    /// Tests whether this task has write access to `slice` as normal memory.
    /// This is used to validate kernel accessses to the memory.
    ///
    /// This is shorthand for `can_access(slice, RegionAttributes::WRITE)`.
    ///
    /// This function is `must_use` because calling it without checking its
    /// return value is incredibly suspicious.
    #[must_use]
    fn can_write<T>(&self, slice: &USlice<T>) -> bool {
        self.can_access(slice, RegionAttributes::WRITE)
    }

    /// Obtains access to the memory backing `slice` as a Rust slice, assuming
    /// that the task `self` can access it for write. This is used to access task
    /// memory from the kernel in validated form.
    pub fn try_write<'a, T>(
        &'a mut self,
        slice: &'a mut USlice<T>,
    ) -> Result<&'a mut [T], FaultInfo>
    where
        T: FromBytes,
    {
        if self.can_write(slice) {
            // Safety: assume_writable requires us to have validated that the
            // slice refers to normal task memory, which we did on the previous
            // line.
            unsafe { Ok(slice.assume_writable()) }
        } else {
            Err(FaultInfo::MemoryAccess {
                address: Some(slice.base_addr() as u32),
                source: FaultSource::Kernel,
            })
        }
    }

    /// Tests whether this task has access to `slice` as normal memory with
    /// *all* of the given access attributes, and none of the forbidden
    /// attributes. This is used to validate kernel accesses to the memory.
    ///
    /// This will refuse access to any memory marked as DEVICE or DMA. This is a
    /// big hammer, as a lot of tasks will probably want to lend memory that is
    /// DMA-capable, and this will block that. It is potentially fixable with
    /// more work. See issue #171.
    ///
    /// You could call this with `atts` as `RegionAttributes::empty()`; this
    /// would just check that memory is not device or available for DMA, and is
    /// a weird thing to do.  A normal call would pass something like
    /// `RegionAttributes::READ`.
    ///
    /// Note that all tasks can "access" any empty slice.
    ///
    /// This function is `must_use` because calling it without checking its
    /// return value is incredibly suspicious.
    #[must_use]
    fn can_access<T>(&self, slice: &USlice<T>, atts: RegionAttributes) -> bool {
        if slice.is_empty() {
            // We deliberately omit tests for empty slices, as they confer no
            // authority as far as the kernel is concerned. This is pretty
            // important because a literal like `&[]` tends to produce a base
            // address of `0 + sizeof::<T>()`, which is almost certainly invalid
            // according to the task's region map... but fine with us.
            return true;
        }
        self.region_table.iter().any(|region| {
            region.covers(slice)
                && region.attributes.contains(atts)
                && !region.attributes.contains(RegionAttributes::DEVICE)
                && !region.attributes.contains(RegionAttributes::DMA)
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

        // We only need to check the mask, and make updates, if the task is
        // ready to hear about notifications.
        if self.state.can_accept_notification() {
            if let Some(firing) = self.take_notifications() {
                // A bit the task is interested in has newly become set!
                // Interrupt it.
                self.save.set_recv_result(TaskId::KERNEL, firing, 0, 0, 0);
                self.state = TaskState::Healthy(SchedState::Runnable);
                return true;
            }
        }
        false
    }

    /// Assuming that this task is in or entering a RECV, inspects the RECV
    /// notification mask argument and compares it to the notification bits. If
    /// if any bits are set in both words, clears those bits in the notification
    /// bits and returns them.
    ///
    /// If the `specific_sender` filter disallows the receipt of kernel
    /// messages, we will treat the notification mask as 0, and you will always
    /// get `None` here.
    ///
    /// This directly accesses the RECV syscall arguments from the task's saved
    /// state, so it doesn't make sense if the task is not performing a RECV --
    /// but this is not checked.
    pub fn take_notifications(&mut self) -> Option<u32> {
        let args = self.save.as_recv_args();
        let ss = args.specific_sender();
        if ss.is_none() || ss == Some(TaskId::KERNEL) {
            // Notifications are not filtered out.
            let firing = self.notifications & args.notification_mask();
            if firing != 0 {
                self.notifications &= !firing;
                return Some(firing);
            }
        }
        None
    }

    /// Checks if this task is in a potentially schedulable state.
    pub fn is_runnable(&self) -> bool {
        self.state == TaskState::Healthy(SchedState::Runnable)
    }

    /// Configures this task's timer.
    ///
    /// `deadline` specifies the moment when the timer should fire, in kernel
    /// time. If `None`, the timer will never fire.
    ///
    /// `notifications` is the set of notification bits to be set when the timer
    /// fires.
    pub fn set_timer(
        &mut self,
        deadline: Option<Timestamp>,
        notifications: NotificationSet,
    ) {
        self.timer.deadline = deadline;
        self.timer.to_post = notifications;
    }

    /// Reads out the state of this task's timer, as previously set by
    /// `set_timer`.
    pub fn timer(&self) -> (Option<Timestamp>, NotificationSet) {
        (self.timer.deadline, self.timer.to_post)
    }

    /// Rewrites this task's state back to its initial form, to effect a task
    /// reboot.
    ///
    /// Note that this only rewrites in-kernel state and relevant parts of
    /// out-of-kernel state (typically, a stack frame stored on the task stack).
    /// This does *not* reinitialize application memory or anything else.
    ///
    /// This does not honor the `START_AT_BOOT` task flag, because this is not a
    /// system reboot. The task will be left in `Stopped` state. If you would
    /// like to run the task after reinitializing it, you must do so explicitly.
    pub fn reinitialize(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.timer = TimerState::default();
        self.notifications = 0;
        self.state = TaskState::default();

        crate::arch::reinitialize(self);
    }

    /// Returns a reference to the `TaskDesc` that was used to initially create
    /// this task.
    pub fn descriptor(&self) -> &'static TaskDesc {
        self.descriptor
    }

    /// Returns a reference to the task's memory region descriptor table.
    pub fn region_table(&self) -> &'static [&'static RegionDesc] {
        self.region_table
    }

    /// Returns this task's current generation number.
    pub fn generation(&self) -> Generation {
        const MASK: u8 = ((1u32 << (16 - TaskId::INDEX_BITS)) - 1) as u8;
        Generation::from(self.generation as u8 & MASK)
    }

    /// Returns this task's priority.
    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Returns a reference to this task's current state, for inspection.
    pub fn state(&self) -> &TaskState {
        &self.state
    }

    /// Alters this task's state from one healthy state to another.
    ///
    /// To deliver a fault, use `force_fault` instead.
    ///
    /// The only currently supported way of getting a task out of fault state is
    /// `reinitialize`. There are a number of invariants that need to be upheld
    /// when a task begins running, and `reinitialize` gives us a place to
    /// centralize them.
    ///
    /// # Panics
    ///
    /// If you attempt to use this to bring a task out of fault state.
    pub fn set_healthy_state(&mut self, s: SchedState) {
        let last = core::mem::replace(&mut self.state, s.into());
        if let TaskState::Faulted { .. } = last {
            panic!();
        }
    }

    /// Returns a reference to the saved machine state for the task.
    pub fn save(&self) -> &crate::arch::SavedState {
        &self.save
    }

    /// Returns a mutable reference to the saved machine state for the task.
    pub fn save_mut(&mut self) -> &mut crate::arch::SavedState {
        &mut self.save
    }
}

/// Interface that must be implemented by the `arch::SavedState` type. This
/// gives architecture-independent access to task state for the rest of the
/// kernel.
///
/// Architectures need to implement the `argX` and `retX` functions plus
/// `syscall_descriptor`, and the rest of the trait (such as the argument proxy
/// types) will just work.
pub trait ArchState: Default {
    /// TODO: this is probably not needed here.
    fn stack_pointer(&self) -> u32;

    /// Reads syscall argument register 0.
    fn arg0(&self) -> u32;
    /// Reads syscall argument register 1.
    fn arg1(&self) -> u32;
    /// Reads syscall argument register 2.
    fn arg2(&self) -> u32;
    /// Reads syscall argument register 3.
    fn arg3(&self) -> u32;
    /// Reads syscall argument register 4.
    fn arg4(&self) -> u32;
    /// Reads syscall argument register 5.
    fn arg5(&self) -> u32;
    /// Reads syscall argument register 6.
    fn arg6(&self) -> u32;

    /// Reads the syscall descriptor (number).
    fn syscall_descriptor(&self) -> u32;

    /// Writes syscall return argument 0.
    fn ret0(&mut self, _: u32);
    /// Writes syscall return argument 1.
    fn ret1(&mut self, _: u32);
    /// Writes syscall return argument 2.
    fn ret2(&mut self, _: u32);
    /// Writes syscall return argument 3.
    fn ret3(&mut self, _: u32);
    /// Writes syscall return argument 4.
    fn ret4(&mut self, _: u32);
    /// Writes syscall return argument 5.
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
    /// arguments for `REPLY_FAULT`.
    fn as_reply_fault_args(&self) -> AsReplyFaultArgs<&Self> {
        AsReplyFaultArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for SET_TIMER.
    fn as_set_timer_args(&self) -> AsSetTimerArgs<&Self> {
        AsSetTimerArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for BORROW_*.
    fn as_borrow_args(&self) -> AsBorrowArgs<&Self> {
        AsBorrowArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for IRQ_CONTROL.
    fn as_irq_args(&self) -> AsIrqArgs<&Self> {
        AsIrqArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for PANIC.
    fn as_panic_args(&self) -> AsPanicArgs<&Self> {
        AsPanicArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for REFRESH_TASK_ID
    fn as_refresh_task_id_args(&self) -> AsRefreshTaskIdArgs<&Self> {
        AsRefreshTaskIdArgs(self)
    }

    /// Returns a proxied reference that assigns names and types to the syscall
    /// arguments for POST
    fn as_post_args(&self) -> AsPostArgs<&Self> {
        AsPostArgs(self)
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
        sender: TaskId,
        operation: u32,
        length: usize,
        response_capacity: usize,
        lease_count: usize,
    ) {
        self.ret0(0); // currently reserved
        self.ret1(u32::from(sender.0));
        self.ret2(operation);
        self.ret3(length as u32);
        self.ret4(response_capacity as u32);
        self.ret5(lease_count as u32);
    }

    /// Sets the response code and length returned from a BORROW_*.
    fn set_borrow_response_and_length(&mut self, resp: u32, len: usize) {
        self.ret0(resp);
        self.ret1(len as u32);
    }

    /// Sets the response code and info returned from BORROW_INFO.
    fn set_borrow_info(&mut self, atts: u32, len: usize) {
        self.ret0(0);
        self.ret1(atts);
        self.ret2(len as u32);
    }

    /// Sets the results of READ_TIMER.
    fn set_time_result(
        &mut self,
        now: Timestamp,
        dl: Option<Timestamp>,
        not: NotificationSet,
    ) {
        let now_u64 = u64::from(now);
        let dl_u64 = dl.map(u64::from).unwrap_or(0);

        self.ret0(now_u64 as u32);
        self.ret1((now_u64 >> 32) as u32);
        self.ret2(dl.is_some() as u32);
        self.ret3(dl_u64 as u32);
        self.ret4((dl_u64 >> 32) as u32);
        self.ret5(not.0);
    }

    /// Sets the results of REFRESH_TASK_ID
    fn set_refresh_task_id_result(&mut self, id: TaskId) {
        self.ret0(id.0 as u32);
    }
}

/// Reference proxy for send argument registers.
pub struct AsSendArgs<T>(T);

impl<'a, T: ArchState> AsSendArgs<&'a T> {
    /// Extracts the task ID the caller wishes to send to.
    pub fn callee(&self) -> TaskId {
        TaskId((self.0.arg0() >> 16) as u16)
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

    /// Gets the task ID we're listening for, or `None` if any sender is
    /// acceptable.
    pub fn specific_sender(&self) -> Option<TaskId> {
        let v = self.0.arg3();
        if v & (1 << 31) != 0 {
            Some(TaskId(v as u16))
        } else {
            None
        }
    }
}

/// Reference proxy for reply argument registers.
pub struct AsReplyArgs<T>(T);

impl<'a, T: ArchState> AsReplyArgs<&'a T> {
    /// Extracts the task ID the caller wishes to reply to.
    pub fn callee(&self) -> TaskId {
        TaskId(self.0.arg0() as u16)
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

/// Reference proxy for `REPLY_FAULT` argument registers.
pub struct AsReplyFaultArgs<T>(T);

impl<'a, T: ArchState> AsReplyFaultArgs<&'a T> {
    /// Extracts the task ID the caller wishes to reply to.
    pub fn callee(&self) -> TaskId {
        TaskId(self.0.arg0() as u16)
    }

    /// Extracts the reason cited.
    pub fn reason(&self) -> Result<ReplyFaultReason, UsageError> {
        ReplyFaultReason::try_from(self.0.arg1())
            .map_err(|_| UsageError::BadReplyFaultReason)
    }
}

/// Reference proxy for SET_TIMER argument registers.
pub struct AsSetTimerArgs<T>(T);

impl<'a, T: ArchState> AsSetTimerArgs<&'a T> {
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

/// Reference proxy for BORROW_* argument registers.
pub struct AsBorrowArgs<T>(T);

impl<'a, T: ArchState> AsBorrowArgs<&'a T> {
    /// Extracts the task being borrowed from.
    pub fn lender(&self) -> TaskId {
        TaskId(self.0.arg0() as u16)
    }

    /// Extracts the lease index.
    pub fn lease_number(&self) -> usize {
        self.0.arg1() as usize
    }

    /// Extracts the intended offset into the borrowed area.
    pub fn offset(&self) -> usize {
        self.0.arg2() as usize
    }
    /// Extracts the caller-side buffer area.
    pub fn buffer(&self) -> Result<USlice<u8>, UsageError> {
        USlice::from_raw(self.0.arg3() as usize, self.0.arg4() as usize)
    }
}

/// Reference proxy for IRQ_CONTROL argument registers.
pub struct AsIrqArgs<T>(T);

impl<'a, T: ArchState> AsIrqArgs<&'a T> {
    /// Bitmask indicating notification bits.
    pub fn notification_bitmask(&self) -> u32 {
        self.0.arg0()
    }

    /// Control word (0=disable, 1=enable)
    pub fn control(&self) -> u32 {
        self.0.arg1()
    }
}

/// Reference proxy for Panic argument registers.
pub struct AsPanicArgs<T>(T);

impl<'a, T: ArchState> AsPanicArgs<&'a T> {
    /// Extracts the task's reported message slice.
    pub fn message(&self) -> Result<USlice<u8>, UsageError> {
        USlice::from_raw(self.0.arg0() as usize, self.0.arg1() as usize)
    }
}

/// Reference proxy for Get Task Generation argument registers.
pub struct AsRefreshTaskIdArgs<T>(T);

impl<'a, T: ArchState> AsRefreshTaskIdArgs<&'a T> {
    pub fn task_id(&self) -> TaskId {
        TaskId(self.0.arg0() as u16)
    }
}

/// Reference proxy for Post argument registers.
pub struct AsPostArgs<T>(T);

impl<'a, T: ArchState> AsPostArgs<&'a T> {
    pub fn task_id(&self) -> TaskId {
        TaskId(self.0.arg0() as u16)
    }

    pub fn notification_bits(&self) -> NotificationSet {
        NotificationSet(self.0.arg1())
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
pub struct NotificationSet(pub u32);

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

/// Checks a user-provided `TaskId` for validity against `table`.
///
/// On success, returns an index that can be used to dereference `table` without
/// panicking.
///
/// On failure, indicates the condition by `UserError`.
pub fn check_task_id_against_table(
    table: &[Task],
    id: TaskId,
) -> Result<usize, UserError> {
    if id.index() >= table.len() {
        return Err(FaultInfo::SyscallUsage(UsageError::TaskOutOfRange).into());
    }

    // Check for dead task ID.
    let table_generation = table[id.index()].generation();

    if table_generation != id.generation() {
        let code = abi::dead_response_code(table_generation);

        return Err(UserError::Recoverable(code, NextTask::Same));
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
///
/// # Panics
///
/// If `previous` is not a valid index in `tasks`.
pub fn priority_scan(
    previous: usize,
    tasks: &[Task],
    pred: impl Fn(&Task) -> bool,
) -> Option<usize> {
    uassert!(previous < tasks.len());
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

/// Puts a task into a forced fault condition.
///
/// The task is designated by the `index` parameter. We need access to the
/// entire task table, as well as the designated task, so that we can take the
/// opportunity to notify the supervisor.
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
pub fn force_fault(
    tasks: &mut [Task],
    index: usize,
    fault: FaultInfo,
) -> NextTask {
    let task = &mut tasks[index];
    task.state = match task.state {
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
    let supervisor_awoken = tasks[0]
        .post(NotificationSet(FAULT_NOTIFICATION.load(Ordering::Relaxed)));
    if supervisor_awoken {
        NextTask::Specific(0)
    } else {
        NextTask::Other
    }
}

/// Produces a current `TaskId` (i.e. one with the correct generation) for
/// `tasks[index]`.
pub fn current_id(tasks: &[Task], index: usize) -> TaskId {
    TaskId::for_index_and_gen(index, tasks[index].generation())
}
