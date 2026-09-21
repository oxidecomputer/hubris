// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! User application support library for Hubris.
//!
//! This contains syscall stubs and types, and re-exports the contents of the
//! `abi` crate that gets shared with the kernel.
//!
//! # Syscall stub implementations
//!
//! Each syscall stub consists of two parts: a public `sys_foo` function
//! intended for use by programs, and an internal `sys_foo_stub` function. This
//! might seem like needless duplication, and in a way, it is.
//!
//! Limitations in the behavior of the current `asm!` feature mean we have a
//! hard time moving values into registers r6, r7, and r11. Because (for better
//! or worse) the syscall ABI uses these registers, we have to take extra steps.
//!
//! The `stub` function contains the actual `asm!` call sequence. It is `naked`,
//! meaning the compiler will *not* attempt to do any framepointer/basepointer
//! nonsense, and we can thus reason about the assignment and availability of
//! all registers.
//!
//! See: https://github.com/rust-lang/rust/issues/73450#issuecomment-650463347

#![no_std]
#![forbid(clippy::wildcard_imports)]

#[macro_use]
pub mod macros;

pub use abi::*;
pub use num_derive::{FromPrimitive, ToPrimitive};
pub use num_traits::{FromPrimitive, ToPrimitive};
pub use unwrap_lite::UnwrapLite;

use crate::arch::{
    BorrowReadArgs, BorrowWriteArgs, RawBorrowInfo, RawRecvMessage,
    RawTimerState, SendArgs,
};
use crate::arch::{
    sys_borrow_info_stub, sys_borrow_read_stub, sys_borrow_write_stub,
    sys_get_timer_stub, sys_irq_control_stub, sys_irq_status_stub,
    sys_panic_stub, sys_post_stub, sys_recv_stub, sys_refresh_task_id_stub,
    sys_reply_fault_stub, sys_reply_stub, sys_send_stub, sys_set_timer_stub,
};
use core::marker::PhantomData;

mod arch;
pub mod hl;
pub mod kipc;
pub mod task_slot;
pub use userlib_units as units;

#[cfg(feature = "critical-section")]
pub mod critical_section;

#[derive(Debug)]
#[repr(transparent)]
pub struct Lease<'a> {
    _kern_rep: abi::ULease,
    _marker: PhantomData<&'a mut ()>,
}

impl<'a> Lease<'a> {
    pub fn read_only(x: &'a [u8]) -> Self {
        Self {
            _kern_rep: abi::ULease {
                attributes: abi::LeaseAttributes::READ,
                base_address: abi::Addr::from_ptr(x.as_ptr()),
                length: x.len(),
            },
            _marker: PhantomData,
        }
    }

    pub fn read_write(x: &'a mut [u8]) -> Self {
        Self {
            _kern_rep: abi::ULease {
                attributes: LeaseAttributes::READ | LeaseAttributes::WRITE,
                base_address: abi::Addr::from_ptr(x.as_mut_ptr()),
                length: x.len(),
            },
            _marker: PhantomData,
        }
    }

    pub fn write_only(x: &'a mut [u8]) -> Self {
        Self {
            _kern_rep: abi::ULease {
                attributes: LeaseAttributes::WRITE,
                base_address: abi::Addr::from_ptr(x.as_mut_ptr()),
                length: x.len(),
            },
            _marker: PhantomData,
        }
    }
}

impl<'a> From<&'a [u8]> for Lease<'a> {
    fn from(x: &'a [u8]) -> Self {
        Self::read_only(x)
    }
}

impl<'a> From<&'a mut [u8]> for Lease<'a> {
    fn from(x: &'a mut [u8]) -> Self {
        Self::read_write(x)
    }
}

#[inline(always)]
pub fn sys_send(
    target: TaskId,
    operation: u16,
    outgoing: &[u8],
    incoming: &mut [u8],
    leases: &[Lease<'_>],
) -> (u32, usize) {
    let mut args = SendArgs {
        packed_target_operation: u32::from(target.0) << 16
            | u32::from(operation),
        outgoing_ptr: outgoing.as_ptr(),
        outgoing_len: outgoing.len(),
        incoming_ptr: incoming.as_mut_ptr(),
        incoming_len: incoming.len(),
        lease_ptr: leases.as_ptr(),
        lease_len: leases.len(),
    };
    unsafe { sys_send_stub(&mut args).into() }
}

/// Performs an "open" RECV that will accept messages from any task or
/// notifications from the kernel.
///
/// The next message sent to this task, or the highest priority message if
/// several are pending simultaneously, will be written into `buffer`, and its
/// information returned.
///
/// `notification_mask` determines which notification bits can interrupt this
/// RECV (any that are 1). If a notification interrupts the RECV, you will get a
/// "message" originating from `TaskId::KERNEL`.
///
/// This operation cannot fail -- it can be interrupted by a notification if you
/// let it, but it always receives _something_.
#[inline(always)]
pub fn sys_recv_open(buffer: &mut [u8], notification_mask: u32) -> RecvMessage {
    match sys_recv(buffer, notification_mask, None) {
        Ok(rm) => rm,
        Err(_) => {
            // Safety: the open-receive version of the syscall is defined as
            // being unable to fail in the kernel ABI, so this path can't happen
            // modulo a kernel bug.
            unsafe { core::hint::unreachable_unchecked() }
        }
    }
}

/// Performs a "closed" RECV that will only accept messages from `sender`.
///
/// The next message sent from `sender` to this task (including a message that
/// has already been sent, but is blocked) will be written into `buffer`, and
/// its information returned.
///
/// `notification_mask` determines which notification bits can interrupt this
/// RECV (any that are 1). To listen _only_ for notifications, pass the `sender`
/// `TaskId::KERNEL`.
///
/// If `sender` is stale (i.e. refers to a deceased generation of the task) when
/// you call this, or if `sender` is rebooted while you're blocked in this
/// operation, this will fail with `ClosedRecvError::Dead`, indicating the
/// `sender`'s new generation (not that a server generally cares).
#[inline(always)]
pub fn sys_recv_closed(
    buffer: &mut [u8],
    notification_mask: u32,
    sender: TaskId,
) -> Result<RecvMessage, ClosedRecvError> {
    sys_recv(buffer, notification_mask, Some(sender)).map_err(|code| {
        // We're not using the extract_new_generation function here because
        // that has a failure code path for cases where the code is not a
        // dead code. In this case, sys_recv is defined as being _only_
        // capable of returning a dead code -- otherwise we have a serious
        // kernel bug. So to avoid the introduction of a panic that can't
        // trigger, we will do this manually:
        ClosedRecvError::Dead(Generation::from(code as u8))
    })
}

/// Things that can go wrong (without faulting) during a closed receive
/// operation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClosedRecvError {
    /// The task you requested to receive from has restarted and the message may
    /// never come.
    Dead(Generation),
}

/// General version of RECV that lets you pick closed vs. open receive at
/// runtime.
///
/// You almost always want `sys_recv_open` or `sys_recv_closed` instead.
#[inline(always)]
pub fn sys_recv(
    buffer: &mut [u8],
    notification_mask: u32,
    specific_sender: Option<TaskId>,
) -> Result<RecvMessage, u32> {
    use core::mem::MaybeUninit;

    // Flatten option into a packed u32; in the C-compatible ABI we provide the
    // task ID in the LSBs, and the "some" flag in the MSB.
    let specific_sender_bits = specific_sender
        .map(|tid| (1u32 << 31) | u32::from(tid.0))
        .unwrap_or(0);
    let mut out = MaybeUninit::<RawRecvMessage>::uninit();
    let rc = unsafe {
        sys_recv_stub(
            buffer.as_mut_ptr(),
            buffer.len(),
            notification_mask,
            specific_sender_bits,
            out.as_mut_ptr(),
        )
    };

    // Safety: stub fully initializes output struct. On failure, it might
    // initialize it with nonsense, but that's okay -- it's still initialized.
    let out = unsafe { out.assume_init() };

    if rc == 0 {
        Ok(RecvMessage {
            sender: TaskId(out.sender as u16),
            operation: out.operation,
            message_len: out.message_len,
            response_capacity: out.response_capacity,
            lease_count: out.lease_count,
        })
    } else {
        Err(rc)
    }
}

/// Bitmask representing notifications from the kernel
///
/// The raw notification bits are available in [`get_raw_bits`], but
/// higher-level functions make it harder to misuse notifications.
#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct NotificationBits(u32);

impl NotificationBits {
    /// Wraps a `u32`
    #[inline]
    pub fn new(bits: u32) -> Self {
        Self(bits)
    }

    /// Returns the raw notification bitmask
    #[inline]
    pub fn get_raw_bits(&self) -> u32 {
        self.0
    }

    /// Checks if a condition signaled by notification bits holds
    ///
    /// The `cond` function should verify the underlying condition, but as an
    /// optimization, it will only be called if any bits set in mask are also
    /// set in `self`. This double-check is important, because notification bits
    /// can potentially be set even if the underlying condition is not true,
    /// such as with extra pended interrupts or inter-task use of `sys_post`.
    ///
    /// If you find yourself passing `|| true` here, see
    /// [`check_notification_mask`].
    #[inline]
    pub fn check_condition<F: Fn() -> bool>(&self, mask: u32, cond: F) -> bool {
        self.check_notification_mask(mask) && cond()
    }

    /// Checks whether the given notification mask is active
    ///
    /// Notifications may occur spuriously; it is recommended to use
    /// `check_condition` instead, if there's a way to verify that the event has
    /// actually occurred.
    #[inline]
    pub fn check_notification_mask(&self, mask: u32) -> bool {
        (self.0 & mask) != 0
    }

    /// Checks whether a timer appears to have gone off, because the timer_mask
    /// bit is set and the kernel timer is no longer set.
    ///
    /// `mask` must the notification bitmask for the timer notification
    #[inline]
    pub fn has_timer_fired(&self, timer_mask: u32) -> bool {
        self.check_condition(timer_mask, || sys_get_timer().deadline.is_none())
    }
}

/// Convenience wrapper for `sys_recv` for the specific, but common, task of
/// listening for notifications. In this specific use, it has the advantage of
/// never panicking and not returning a `Result` that must be checked.
#[inline(always)]
pub fn sys_recv_notification(notification_mask: u32) -> NotificationBits {
    match sys_recv(&mut [], notification_mask, Some(TaskId::KERNEL)) {
        Ok(rm) => {
            // The notification bits come back from the kernel in the operation
            // code field.
            NotificationBits(rm.operation)
        }
        Err(_) => {
            // Safety: Because we passed Some(TaskId::KERNEL), this is defined
            // as not being able to happen.
            unsafe { core::hint::unreachable_unchecked() }
        }
    }
}

pub struct RecvMessage {
    pub sender: TaskId,
    pub operation: u32,
    pub message_len: usize,
    pub response_capacity: usize,
    pub lease_count: usize,
}

#[inline(always)]
pub fn sys_reply(peer: TaskId, code: u32, message: &[u8]) {
    unsafe {
        sys_reply_stub(peer.0 as u32, code, message.as_ptr(), message.len())
    }
}

/// Sets this task's timer.
///
/// The timer is set to `deadline`. If `deadline` is `None`, the timer is
/// disabled. Otherwise, the timer is configured to notify when the specified
/// time (in ticks since boot) is reached. When that occurs, the `notifications`
/// will get posted to this task, and the timer will be disabled.
///
/// If the deadline is chosen such that the timer *would have already fired*,
/// had it been set earlier -- that is, if the deadline is `<=` the current time
/// -- the `notifications` will be posted immediately and the timer will not be
/// enabled.
#[inline(always)]
pub fn sys_set_timer(deadline: Option<u64>, notifications: u32) {
    let raw_deadline = deadline.unwrap_or(0);
    unsafe {
        sys_set_timer_stub(
            deadline.is_some() as u32,
            raw_deadline as u32,
            (raw_deadline >> 32) as u32,
            notifications,
        )
    }
}

/// Convenience wrapper for `sys_set_timer` that sets a point in time relative
/// to whatever the current kernel timestamp happens to be.
///
/// This is only useful for setting intervals up to 49.7 days. In practice
/// our intervals tend to be much shorter than this. This restriction exists to
/// avoid the possibility of overflow: overflow will start happening only within
/// 49.7 days of the "end of time" in 584 million years.
///
/// Returns the actual computed wake time for your reference.
pub fn set_timer_relative(interval: u32, notifications: u32) -> u64 {
    // wrapping add because the uptime is likely to be less than 584 million
    // years.
    let wake = sys_get_timer().now.wrapping_add(u64::from(interval));
    sys_set_timer(Some(wake), notifications);
    wake
}

#[inline(always)]
pub fn sys_borrow_read(
    lender: TaskId,
    index: usize,
    offset: usize,
    dest: &mut [u8],
) -> (u32, usize) {
    let mut args = BorrowReadArgs {
        lender: lender.0 as u32,
        index,
        offset,
        dest: dest.as_mut_ptr(),
        dest_len: dest.len(),
    };
    unsafe { sys_borrow_read_stub(&mut args).into() }
}

#[inline(always)]
pub fn sys_borrow_write(
    lender: TaskId,
    index: usize,
    offset: usize,
    src: &[u8],
) -> (u32, usize) {
    let mut args = BorrowWriteArgs {
        lender: lender.0 as u32,
        index,
        offset,
        src: src.as_ptr(),
        src_len: src.len(),
    };
    unsafe { sys_borrow_write_stub(&mut args).into() }
}

#[inline(always)]
pub fn sys_borrow_info(lender: TaskId, index: usize) -> Option<BorrowInfo> {
    use core::mem::MaybeUninit;

    let mut raw = MaybeUninit::<RawBorrowInfo>::uninit();
    unsafe {
        sys_borrow_info_stub(lender.0 as u32, index, raw.as_mut_ptr());
    }
    // Safety: stub completely initializes record
    let raw = unsafe { raw.assume_init() };

    if raw.rc == 0 {
        Some(BorrowInfo {
            attributes: abi::LeaseAttributes::from_bits_truncate(raw.atts),
            len: raw.length,
        })
    } else {
        None
    }
}

/// Information record returned by `sys_borrow_info`.
pub struct BorrowInfo {
    /// Attributes of the lease.
    pub attributes: abi::LeaseAttributes,
    /// Length of borrowed memory, in bytes.
    pub len: usize,
}

#[inline(always)]
pub fn sys_irq_control(mask: u32, enable: bool) {
    let mut arg = IrqControlArg::empty();
    if enable {
        arg |= IrqControlArg::ENABLED;
    }

    unsafe {
        sys_irq_control_stub(mask, arg.bits());
    }
}

/// Variation on [`sys_irq_control`] that also clears any pending interrupt.
///
/// This sets the interrupt enable status based on `enable`, and also cancels a
/// pending instance of this interrupt in the interrupt controller, if the
/// interrupt controller supports such a concept (ARM M-profile NVIC does, for
/// instance).
#[inline(always)]
pub fn sys_irq_control_clear_pending(mask: u32, enable: bool) {
    let mut arg = IrqControlArg::CLEAR_PENDING;
    if enable {
        arg |= IrqControlArg::ENABLED;
    }
    unsafe {
        sys_irq_control_stub(mask, arg.bits());
    }
}

#[inline(always)]
pub fn sys_panic(msg: &[u8]) -> ! {
    unsafe { sys_panic_stub(msg.as_ptr(), msg.len()) }
}

/// Reads the state of this task's timer.
///
/// This returns three values in a `TimerState` struct:
///
/// - `now` is the current time on the timer, in ticks since boot.
/// - `deadline` is either `None`, meaning the timer notifications are disabled,
///   or `Some(t)`, meaning the timer will post notifications at time `t`.
/// - `on_dl` are the notification bits that will be posted on deadline.
///
/// `deadline` and `on_dl` are as configured by `sys_set_timer`.
///
/// `now` is monotonically advancing and can't be changed.
#[inline(always)]
pub fn sys_get_timer() -> TimerState {
    use core::mem::MaybeUninit;

    let mut out = MaybeUninit::<RawTimerState>::uninit();
    unsafe {
        sys_get_timer_stub(out.as_mut_ptr());
    }
    // Safety: stub fully initializes output struct.
    let out = unsafe { out.assume_init() };

    TimerState {
        now: u64::from(out.now_lo) | u64::from(out.now_hi) << 32,
        deadline: if out.set != 0 {
            Some(u64::from(out.dl_lo) | u64::from(out.dl_hi) << 32)
        } else {
            None
        },
        on_dl: out.on_dl,
    }
}

/// Result of `sys_get_timer`, provides information about task timer state.
pub struct TimerState {
    /// Current task timer time, in ticks.
    pub now: u64,
    /// Current deadline, or `None` if the deadline is not pending.
    pub deadline: Option<u64>,
    /// Notifications to be delivered if the deadline is reached.
    pub on_dl: u32,
}

// Make the no-panic and panic-messages features mutually exclusive.
#[cfg(all(feature = "no-panic", feature = "panic-messages"))]
compile_error!(
    "Both the userlib/panic-messages and userlib/no-panic feature flags \
     are set! This doesn't make a lot of sense and is probably not what \
     you wanted. (If you have a use case for this combination, update \
     this check in userlib.)"
);

/// Maximum length (in bytes) of the panic message string captured when the
/// `panic-messages` feature is enabled. Panics which format messages longer
/// than this many bytes are truncated to this length.
///
/// There's a tradeoff here between "getting a useful message" and "wasting a
/// lot of RAM." Somewhat arbitrarily, we choose to collect this many bytes
/// of panic message (and permanently reserve the same number of bytes of
/// RAM):
pub const PANIC_MESSAGE_MAX_LEN: usize = 128;

#[inline(always)]
pub fn sys_refresh_task_id(task_id: TaskId) -> TaskId {
    let tid = unsafe { sys_refresh_task_id_stub(task_id.0 as u32) };
    TaskId(tid as u16)
}

#[inline(always)]
pub fn sys_post(task_id: TaskId, bits: u32) -> u32 {
    unsafe { sys_post_stub(task_id.0 as u32, bits) }
}

#[inline(always)]
pub fn sys_reply_fault(task_id: TaskId, reason: ReplyFaultReason) {
    unsafe { sys_reply_fault_stub(task_id.0 as u32, reason as u32) }
}

/// Returns the current status of any interrupts mapped to the provided
/// notification mask.
///
/// # Arguments
///
/// - `mask`: a notification mask for interrupts mapped to the current task.
///
/// # Returns
///
/// An [`IrqStatus`] (see the `abi` crate) describing the status of the
/// interrupts in the notification mask.
///
/// # Faults
///
/// This syscall faults the caller if the given notification bitmask is not
/// mapped to an interrupt in this task.
#[inline(always)]
pub fn sys_irq_status(mask: u32) -> abi::IrqStatus {
    let status = unsafe { sys_irq_status_stub(mask) };
    abi::IrqStatus::from_bits_truncate(status)
}
