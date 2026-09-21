// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Target-specific syscall implementations.
//!
//! In practice, this works by
//!
//! - Conditionally defining a nested module (below).
//! - `pub use`-ing its contents
//! - Naming that module's implementation of the [`Arch`] trait as
//!   [`Current`].
//!
//! The syscalls every target must provide are defined by the [`Arch`] trait,
//! and the crate root wraps each of them in a public `sys_*` function. The
//! remaining items each target module provides are free items, because they
//! only make sense on one target:
//!
//! - On a Hubris target, `_start`, which the crate root re-exports, and the
//!   panic handlers.
//! - On the host, `resolve_task_slot`, which `task_slot` uses to look up
//!   task indices that a Hubris build would have patched into the image.
//!
//! Everything shared between implementations (argument types, results,
//! convenience wrappers) lives in the crate root instead.

use abi::{IrqStatus, ReplyFaultReason, TaskId};

use crate::{BorrowInfo, Lease, RecvMessage, TimerState};

cfg_if::cfg_if! {
    if #[cfg(target_os = "none")] {
        mod thumb;
        pub use thumb::*;

        /// The target the task is being built for.
        pub type Current = thumb::Thumb;
    } else {
        mod host;
        pub use host::*;

        /// The target the task is being built for.
        pub type Current = host::Host;
    }
}

/// Syscalls a task can make, as implemented for the target it runs on.
///
/// Implemented by a zero-sized type in each target module, and reached
/// through [`Current`]. Each method implements the crate root's function of
/// the same name with a `sys_` prefix, which documents its behavior.
pub trait Arch {
    /// Implements [`crate::sys_send`].
    fn send(
        target: TaskId,
        operation: u16,
        outgoing: &[u8],
        incoming: &mut [u8],
        leases: &[Lease<'_>],
    ) -> (u32, usize);

    /// Implements [`crate::sys_recv`].
    fn recv(
        buffer: &mut [u8],
        notification_mask: u32,
        specific_sender: Option<TaskId>,
    ) -> Result<RecvMessage, u32>;

    /// Implements [`crate::sys_reply`].
    fn reply(peer: TaskId, code: u32, message: &[u8]);

    /// Implements [`crate::sys_set_timer`].
    fn set_timer(deadline: Option<u64>, notifications: u32);

    /// Implements [`crate::sys_borrow_read`].
    fn borrow_read(
        lender: TaskId,
        index: usize,
        offset: usize,
        dest: &mut [u8],
    ) -> (u32, usize);

    /// Implements [`crate::sys_borrow_write`].
    fn borrow_write(
        lender: TaskId,
        index: usize,
        offset: usize,
        src: &[u8],
    ) -> (u32, usize);

    /// Implements [`crate::sys_borrow_info`].
    fn borrow_info(lender: TaskId, index: usize) -> Option<BorrowInfo>;

    /// Implements [`crate::sys_irq_control`].
    fn irq_control(mask: u32, enable: bool);

    /// Implements [`crate::sys_irq_control_clear_pending`].
    fn irq_control_clear_pending(mask: u32, enable: bool);

    /// Implements [`crate::sys_panic`].
    fn panic(msg: &[u8]) -> !;

    /// Implements [`crate::sys_get_timer`].
    fn get_timer() -> TimerState;

    /// Implements [`crate::sys_refresh_task_id`].
    fn refresh_task_id(task_id: TaskId) -> TaskId;

    /// Implements [`crate::sys_post`].
    fn post(task_id: TaskId, bits: u32) -> u32;

    /// Implements [`crate::sys_reply_fault`].
    fn reply_fault(task_id: TaskId, reason: ReplyFaultReason);

    /// Implements [`crate::sys_irq_status`].
    fn irq_status(mask: u32) -> IrqStatus;
}
