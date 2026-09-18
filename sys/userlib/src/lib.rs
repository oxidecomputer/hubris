// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! User application support library for Hubris.
//!
//! This contains syscall stubs and types, and re-exports the contents of the
//! `abi` crate that gets shared with the kernel.
//!
//! The syscall entry points (`sys_send`, `sys_recv`, and friends) are provided
//! by a target-specific implementation in the `arch` module and re-exported
//! here. The types they exchange, and the convenience wrappers built on top of
//! them, live in this file and are shared by every implementation.

#![no_std]
#![forbid(clippy::wildcard_imports)]

#[macro_use]
pub mod macros;

pub use abi::*;
pub use num_derive::{FromPrimitive, ToPrimitive};
pub use num_traits::{FromPrimitive, ToPrimitive};
pub use unwrap_lite::UnwrapLite;

use core::marker::PhantomData;

mod arch;
pub mod hl;
pub mod kipc;
pub mod task_slot;
pub use userlib_units as units;

#[cfg(feature = "critical-section")]
pub mod critical_section;

#[doc(hidden)]
pub use arch::_start;
pub use arch::{
    sys_borrow_info, sys_borrow_read, sys_borrow_write, sys_get_timer,
    sys_irq_control, sys_irq_control_clear_pending, sys_irq_status, sys_panic,
    sys_post, sys_recv, sys_refresh_task_id, sys_reply, sys_reply_fault,
    sys_send, sys_set_timer,
};

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
                base_address: abi::Addr::from_ptr(x.as_ptr()),
                length: x.len(),
            },
            _marker: PhantomData,
        }
    }

    pub fn write_only(x: &'a mut [u8]) -> Self {
        Self {
            _kern_rep: abi::ULease {
                attributes: LeaseAttributes::WRITE,
                base_address: abi::Addr::from_ptr(x.as_ptr()),
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

/// Information record returned by `sys_borrow_info`.
pub struct BorrowInfo {
    /// Attributes of the lease.
    pub attributes: abi::LeaseAttributes,
    /// Length of borrowed memory, in bytes.
    pub len: usize,
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
