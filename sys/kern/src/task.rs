// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Implementation of tasks.

use core::ops::Range;

use abi::{
    FaultInfo, FaultSource, Generation, ReplyFaultReason, SchedState, TaskId,
    TaskState, ULease, UsageError,
};
use zerocopy::{FromBytes, Immutable, KnownLayout};

use crate::descs::{
    Priority, RegionAttributes, RegionDesc, TaskDesc, TaskFlags,
    REGIONS_PER_TASK,
};
use crate::err::UserError;
use crate::startup::HUBRIS_FAULT_NOTIFICATION;
use crate::time::Timestamp;
use crate::umem::USlice;

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

    /// Notification status.
    notifications: u32,

    /// Pointer to the ROM descriptor used to create this task, so it can be
    /// restarted.
    descriptor: &'static TaskDesc,

    /// Stack watermark tracking support.
    ///
    /// This field is completely missing if the feature is disabled to make that
    /// clear to debug tools.
    #[cfg(feature = "stack-watermark")]
    stack_watermark: StackWatermark,
}

impl Task {
    /// Creates a `Task` in its initial state, filling in fields from
    /// `descriptor`.
    pub fn from_descriptor(descriptor: &'static TaskDesc) -> Self {
        Task {
            priority: Priority(descriptor.priority),
            state: if descriptor.flags.contains(TaskFlags::START_AT_BOOT) {
                TaskState::Healthy(SchedState::Runnable)
            } else {
                TaskState::default()
            },

            descriptor,

            generation: 0,
            notifications: 0,
            save: crate::arch::SavedState::default(),
            timer: crate::task::TimerState::default(),
            #[cfg(feature = "stack-watermark")]
            stack_watermark: StackWatermark::default(),
        }
    }

    /// Tests whether this task has read access to `slice` as normal memory.
    /// This is used to validate kernel accessses to the memory.
    ///
    /// This is shorthand for `can_access(slice, READ, DMA)`.
    ///
    /// This function is `must_use` because calling it without checking its
    /// return value is incredibly suspicious.
    #[must_use]
    fn can_read<T>(&self, slice: &USlice<T>) -> bool {
        self.can_access(slice, RegionAttributes::READ, RegionAttributes::DMA)
    }

    /// Obtains access to the memory backing `slice` as a Rust slice, assuming
    /// that the task `self` can access it for read. This is used to access task
    /// memory from the kernel in validated form.
    ///
    /// This will treat memory marked `DEVICE` or `DMA` as inaccessible; see
    /// `can_access` for more details.
    pub fn try_read<'a, T>(
        &'a self,
        slice: &'a USlice<T>,
    ) -> Result<&'a [T], FaultInfo>
    where
        T: FromBytes + Immutable + KnownLayout,
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

    /// Obtains access to the memory backing `slice` as a Rust raw pointer
    /// range, if and only if the task `self` can access it for read. This is
    /// used to access task memory from the kernel in validated form.
    ///
    /// Because the result of this function is not a Rust slice, this can be
    /// used to interact with memory marked as `DMA` -- that is, normal memory
    /// that might be asynchronously modified (from the perspective of the CPU).
    /// If you want to access memory using a proper Rust slice, use `try_read`
    /// instead.
    ///
    /// Like `try_read` this will treat memory marked `DEVICE` as inaccessible;
    /// see `can_access` for more details.
    pub fn try_read_dma<'a, T>(
        &'a self,
        slice: &'a USlice<T>,
    ) -> Result<Range<*const T>, FaultInfo>
    where
        T: FromBytes + Immutable + KnownLayout,
    {
        if self.can_access(
            slice,
            RegionAttributes::READ,
            RegionAttributes::empty(),
        ) {
            // Safety: assume_readable_raw requires us to have validated that
            // the slice refers to normal task memory, which we did on the
            // previous line.
            unsafe { Ok(slice.assume_readable_raw()) }
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
    /// This is shorthand for `can_access(slice, WRITE, DMA)`.
    ///
    /// This function is `must_use` because calling it without checking its
    /// return value is incredibly suspicious.
    #[must_use]
    fn can_write<T>(&self, slice: &USlice<T>) -> bool {
        self.can_access(slice, RegionAttributes::WRITE, RegionAttributes::DMA)
    }

    /// Obtains access to the memory backing `slice` as a Rust slice, assuming
    /// that the task `self` can access it for write. This is used to access task
    /// memory from the kernel in validated form.
    ///
    /// This will treat memory marked `DEVICE` or `DMA` as inaccessible; see
    /// `can_access` for more details.
    pub fn try_write<'a, T>(
        &'a mut self,
        slice: &'a mut USlice<T>,
    ) -> Result<&'a mut [T], FaultInfo>
    where
        T: FromBytes + Immutable + KnownLayout,
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
    /// *all* of the given `desired` attributes, and none of the `forbidden`
    /// attributes. This is used to validate kernel accesses to the memory.
    ///
    /// In addition to the `forbidden` attributes passed by the caller, this
    /// will also refuse to access memory marked as `DEVICE`, because such
    /// accesses may be side effecting.
    ///
    /// Most uses of this function also forbid `DMA`, because it is not sound to
    /// create Rust references into `DMA` memory. Access to `DMA` memory is
    /// possible but must use raw pointers and tolerate potential races. (Task
    /// dumps are one of the only cases where this really makes sense.)
    ///
    /// You could call this with `desired` as `RegionAttributes::empty()`; this
    /// would just check that memory is not device, and is a weird thing to do.
    /// A normal call would pass something like `RegionAttributes::READ`.
    ///
    /// Note that all tasks can "access" any empty slice.
    ///
    /// This function is `must_use` because calling it without checking its
    /// return value is incredibly suspicious.
    #[must_use]
    fn can_access<T>(
        &self,
        slice: &USlice<T>,
        desired: RegionAttributes,
        forbidden: RegionAttributes,
    ) -> bool {
        // Forceably include DEVICE in the forbidden set, whether or not the
        // caller thought about it.
        let forbidden = forbidden | RegionAttributes::DEVICE;

        // Delegate the actual tests to the kerncore crate, but with our
        // attribute-sensing customization:
        kerncore::can_access(slice, self.region_table(), |region| {
            region.attributes.contains(desired)
                && !region.attributes.intersects(forbidden)
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
    /// This directly accesses the RECV syscall arguments from the task's saved
    /// state, so it doesn't make sense if the task is not performing a RECV --
    /// but this is not checked.
    pub fn take_notifications(&mut self) -> Option<u32> {
        let args = self.save.as_recv_args();

        let firing = self.notifications & args.notification_mask;
        if firing != 0 {
            self.notifications &= !firing;
            Some(firing)
        } else {
            None
        }
    }

    /// Returns `true` if any of the notification bits in `mask` are set in this
    /// task's notification set.
    ///
    /// This does *not* clear any bits in the task's notification set.
    pub fn has_notifications(&self, mask: u32) -> bool {
        self.notifications & mask != 0
    }

    /// Checks if this task is in a potentially schedulable state.
    pub fn is_runnable(&self) -> bool {
        matches!(self.state, TaskState::Healthy(SchedState::Runnable))
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

        #[cfg(feature = "stack-watermark")]
        {
            self.stack_watermark.past_low = u32::min(
                self.stack_watermark.past_low,
                self.stack_watermark.current_low,
            );
            self.stack_watermark.current_low = u32::MAX;
        }

        crate::arch::reinitialize(self);
    }

    /// Updates the task's stack watermark stats, if enabled.
    ///
    /// If not enabled, this does nothing, so it should be safe to call freely
    /// without checking for the feature.
    pub fn update_stack_watermark(&mut self) {
        #[cfg(feature = "stack-watermark")]
        {
            self.stack_watermark.current_low = u32::min(
                self.stack_watermark.current_low,
                self.save().stack_pointer(),
            );
        }
    }

    /// Returns a reference to the `TaskDesc` that was used to initially create
    /// this task.
    pub fn descriptor(&self) -> &'static TaskDesc {
        self.descriptor
    }

    /// Returns a reference to the task's memory region descriptor table.
    pub fn region_table(&self) -> &[&'static RegionDesc; REGIONS_PER_TASK] {
        &self.descriptor.regions
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

#[cfg(feature = "stack-watermark")]
#[derive(Copy, Clone, Debug)]
struct StackWatermark {
    /// Tracks the lowest stack pointer value (e.g. fullest stack) observed on
    /// any kernel entry for this instance of this task.
    ///
    /// Initialized to `u32::MAX` if the task has not yet run.
    current_low: u32,

    /// Tracks the lowest stack pointer value (e.g. fullest stack) observed on
    /// any kernel entry across *any* instance of this task.
    ///
    /// Initialized to `u32::MAX` if the task has not yet run.
    past_low: u32,
}

#[cfg(feature = "stack-watermark")]
impl Default for StackWatermark {
    fn default() -> Self {
        Self {
            current_low: u32::MAX,
            past_low: u32::MAX,
        }
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

    /// Interprets arguments as for the SEND syscall and returns the results.
    ///
    /// This is inlined because it's called from several places, and most of
    /// those places only use _part_ of its result -- so inlining it lets most
    /// of its code be eliminated and makes text smaller.
    #[inline(always)]
    fn as_send_args(&self) -> SendArgs {
        SendArgs {
            callee: TaskId((self.arg0() >> 16) as u16),
            operation: self.arg0() as u16,
            message: USlice::from_raw(
                self.arg1() as usize,
                self.arg2() as usize,
            ),
            response: USlice::from_raw(
                self.arg3() as usize,
                self.arg4() as usize,
            ),
            lease_table: USlice::from_raw(
                self.arg5() as usize,
                self.arg6() as usize,
            ),
        }
    }

    /// Interprets arguments as for the RECV syscall and returns the results.
    ///
    /// This is inlined because it's called from several places, and most of
    /// those places only use _part_ of its result -- so inlining it lets most
    /// of its code be eliminated and makes text smaller.
    #[inline(always)]
    fn as_recv_args(&self) -> RecvArgs {
        RecvArgs {
            buffer: USlice::from_raw(
                self.arg0() as usize,
                self.arg1() as usize,
            ),
            notification_mask: self.arg2(),
            specific_sender: {
                let v = self.arg3();
                if v & (1 << 31) != 0 {
                    Some(TaskId(v as u16))
                } else {
                    None
                }
            },
        }
    }

    /// Interprets arguments as for the REPLY syscall and returns the results.
    fn as_reply_args(&self) -> ReplyArgs {
        ReplyArgs {
            callee: TaskId(self.arg0() as u16),
            response_code: self.arg1(),
            message: USlice::from_raw(
                self.arg2() as usize,
                self.arg3() as usize,
            ),
        }
    }

    /// Interprets arguments as for the `REPLY_FAULT` syscall and returns the
    /// results.
    fn as_reply_fault_args(&self) -> ReplyFaultArgs {
        ReplyFaultArgs {
            callee: TaskId(self.arg0() as u16),
            reason: ReplyFaultReason::try_from(self.arg1())
                .map_err(|_| UsageError::BadReplyFaultReason),
        }
    }

    /// Interprets arguments as for the `SET_TIMER` syscall and returns the
    /// results.
    fn as_set_timer_args(&self) -> SetTimerArgs {
        SetTimerArgs {
            deadline: if self.arg0() != 0 {
                Some(Timestamp::from(
                    u64::from(self.arg2()) << 32 | u64::from(self.arg1()),
                ))
            } else {
                None
            },
            notification: NotificationSet(self.arg3()),
        }
    }

    /// Interprets arguments as for the `BORROW_*` family of syscalls and
    /// returns the result.
    fn as_borrow_args(&self) -> BorrowArgs {
        BorrowArgs {
            lender: TaskId(self.arg0() as u16),
            lease_number: self.arg1() as usize,
            offset: self.arg2() as usize,
            buffer: USlice::from_raw(
                self.arg3() as usize,
                self.arg4() as usize,
            ),
        }
    }

    /// Interprets arguments as for the `IRQ_CONTROL` syscall and returns the
    /// results.
    fn as_irq_args(&self) -> IrqArgs {
        IrqArgs {
            notification_bitmask: self.arg0(),
            control: self.arg1(),
        }
    }

    /// Interprets arguments as for the `PANIC` syscall and returns the results.
    fn as_panic_args(&self) -> PanicArgs {
        PanicArgs {
            message: USlice::from_raw(
                self.arg0() as usize,
                self.arg1() as usize,
            ),
        }
    }

    /// Interprets arguments as for the `REFRESH_TASK_ID` syscall and returns
    /// the results.
    fn as_refresh_task_id_args(&self) -> RefreshTaskIdArgs {
        RefreshTaskIdArgs {
            task_id: TaskId(self.arg0() as u16),
        }
    }

    /// Interprets arguments as for the `POST` syscall and returns the results.
    fn as_post_args(&self) -> PostArgs {
        PostArgs {
            task_id: TaskId(self.arg0() as u16),
            notification_bits: NotificationSet(self.arg1()),
        }
    }

    /// Interprets arguments as for the `IRQ_STATUS` syscall and returns the results.
    fn as_irq_status_args(&self) -> IrqStatusArgs {
        IrqStatusArgs {
            notification_bitmask: self.arg0(),
        }
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

    /// Sets the results of IRQ_STATUS.
    fn set_irq_status_result(&mut self, status: abi::IrqStatus) {
        self.ret0(status.bits());
    }
}

/// Decoded arguments for the `SEND` syscall.
#[derive(Clone, Debug)]
pub struct SendArgs {
    pub callee: TaskId,
    pub operation: u16,
    pub message: Result<USlice<u8>, UsageError>,
    pub response: Result<USlice<u8>, UsageError>,
    pub lease_table: Result<USlice<ULease>, UsageError>,
}

/// Decoded arguments for the `RECV` syscall.
#[derive(Clone, Debug)]
pub struct RecvArgs {
    pub buffer: Result<USlice<u8>, UsageError>,
    pub notification_mask: u32,
    pub specific_sender: Option<TaskId>,
}

/// Decoded arguments for the `REPLY` syscall.
#[derive(Clone, Debug)]
pub struct ReplyArgs {
    pub callee: TaskId,
    pub response_code: u32,
    pub message: Result<USlice<u8>, UsageError>,
}

/// Decoded arguments for the `REPLY_FAULT` syscall.
#[derive(Clone, Debug)]
pub struct ReplyFaultArgs {
    pub callee: TaskId,
    pub reason: Result<ReplyFaultReason, UsageError>,
}

/// Decoded arguments for the `SET_TIMER` syscall.
#[derive(Clone, Debug)]
pub struct SetTimerArgs {
    pub deadline: Option<Timestamp>,
    pub notification: NotificationSet,
}

/// Decoded arguments for the `BORROW_*` syscalls.
#[derive(Clone, Debug)]
pub struct BorrowArgs {
    pub lender: TaskId,
    pub lease_number: usize,
    pub offset: usize,
    pub buffer: Result<USlice<u8>, UsageError>,
}

/// Decoded arguments for the `IRQ_CONTROL` syscall.
#[derive(Clone, Debug)]
pub struct IrqArgs {
    pub notification_bitmask: u32,
    pub control: u32,
}

/// Decoded arguments for the `PANIC` syscall.
#[derive(Clone, Debug)]
pub struct PanicArgs {
    pub message: Result<USlice<u8>, UsageError>,
}

/// Decoded arguments for the `REFRESH_TASK_ID` syscall.
#[derive(Clone, Debug)]
pub struct RefreshTaskIdArgs {
    pub task_id: TaskId,
}

/// Decoded arguments for the `POST` syscall.
#[derive(Clone, Debug)]
pub struct PostArgs {
    pub task_id: TaskId,
    pub notification_bits: NotificationSet,
}

/// Decoded arguments for the `IRQ_STATUS` syscall.
#[derive(Clone, Debug)]
pub struct IrqStatusArgs {
    pub notification_bitmask: u32,
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

    Ok(id.index())
}

/// Selects a new task to run after `previous`. Tries to be fair, kind of.
///
/// If no tasks are runnable, the kernel panics.
pub fn select(previous: usize, tasks: &[Task]) -> &Task {
    match priority_scan(previous, tasks, |t| t.is_runnable()) {
        Some((_index, task)) => task,
        None => panic!(),
    }
}

/// Scans the task table to find a prioritized candidate.
///
/// Scans `tasks` for the next task, after `previous`, that satisfies `pred`. If
/// more than one task satisfies `pred`, returns the most important one. If
/// multiple tasks with the same priority satisfy `pred`, prefers the first one
/// in order after `previous`, mod `tasks.len()`. Finally, if no tasks satisfy
/// `pred`, returns `None`
///
/// Whew.
///
/// This is generally the right way to search a task table, and is used to
/// implement (among other bits) the scheduler.
///
/// On success, the return value is the task's index in the task table, and a
/// direct reference to the task.
pub fn priority_scan(
    previous: usize,
    tasks: &[Task],
    pred: impl Fn(&Task) -> bool,
) -> Option<(usize, &Task)> {
    let mut pos = previous;
    let mut choice: Option<(usize, &Task)> = None;
    for _step_no in 0..tasks.len() {
        pos = pos.wrapping_add(1);
        if pos >= tasks.len() {
            pos = 0;
        }
        let t = &tasks[pos];
        if !pred(t) {
            continue;
        }

        if let Some((_, best_task)) = choice {
            if !t.priority.is_more_important_than(best_task.priority) {
                continue;
            }
        }

        choice = Some((pos, t));
    }

    choice
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
    let supervisor_awoken =
        tasks[0].post(NotificationSet(HUBRIS_FAULT_NOTIFICATION));
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
