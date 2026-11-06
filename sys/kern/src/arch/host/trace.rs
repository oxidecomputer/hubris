// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tracing output for the kernel
//!
//! This is usable for observing the behavior in the host-native environment.
//!
//! Currently, this is pretty basic `strace`-like output, printing a line for
//! each system call that occurs.

use crate::arch::host::config::{TraceFilter, options};
use crate::arch::host::{Call, task_name_from_idx};
use abi::{Sysnum, TaskId};

fn trace_filter() -> &'static TraceFilter {
    &options().trace
}

pub(crate) fn tracing() -> bool {
    !matches!(trace_filter(), TraceFilter::Off)
}

/// Whether a syscall involving these tasks (the caller, and any peer) should
/// be traced.
pub(crate) fn traced(participants: &[usize]) -> bool {
    match trace_filter() {
        TraceFilter::Off => false,
        TraceFilter::All => true,
        TraceFilter::Tasks(indices) => {
            participants.iter().any(|i| indices.contains(i))
        }
    }
}

/// Print a log line to stderr, bypassing trace filtering checks.
macro_rules! event {
    ($($arg:tt)*) => {
        eprintln!("[kernel t={:07}] {}", u64::from(Host::now()),
            format_args!($($arg)*));
    };
}

/// Print a log line to stderr, for a syscall by `index` involving
/// `participants`.
///
/// Respects trace filtering checks
macro_rules! trace_task {
    ($participants:expr, $($arg:tt)*) => {
        if crate::arch::host::trace::traced($participants) {
            eprintln!("[kernel t={:07}] {}", u64::from(Host::now()),
                format_args!($($arg)*));
        }
    };
}

/// Describes a syscall for the trace. Returns the text and the peer task, if
/// any.
pub(crate) fn describe_syscall(call: &Call) -> (String, Option<usize>) {
    let [a0, a1, a2, a3, ..] = call.args;
    let input = call.input().unwrap();
    let message: &[u8] = unsafe { input.assume_readable() };

    match call.syscall_nr {
        Sysnum::Send => {
            let peer = TaskId((a0 >> 16) as u16).index();
            let operation = a0 as u16;
            let leases = match call.lease_table.len() {
                0 => String::new(),
                n => format!(" [{n} leases]"),
            };
            (
                format!(
                    "SEND to {}: op {operation} ({} bytes){leases}",
                    task_name_from_idx(peer),
                    message.len()
                ),
                Some(peer),
            )
        }
        Sysnum::Reply => {
            let peer = TaskId(a0 as u16).index();
            (
                format!(
                    "REPLY to {}: code {a1} ({} bytes)",
                    task_name_from_idx(peer),
                    message.len()
                ),
                Some(peer),
            )
        }
        Sysnum::ReplyFault => {
            let peer = TaskId(a0 as u16).index();
            (
                format!(
                    "REPLY_FAULT to {}: reason {a1}",
                    task_name_from_idx(peer)
                ),
                Some(peer),
            )
        }
        Sysnum::Recv => {
            let sender =
                (a3 & (1 << 31) != 0).then(|| TaskId(a3 as u16).index());
            (format!("RECV mask {a2:#x} from {sender:?}"), None)
        }
        Sysnum::SetTimer => {
            let deadline =
                (a0 != 0).then(|| u64::from(a2) << 32 | u64::from(a1));
            (format!("SET_TIMER {deadline:?} bits {a3:#x}"), None)
        }
        Sysnum::Panic => {
            (format!("PANIC: {}", String::from_utf8_lossy(message)), None)
        }
        _ => (syscall_name(call.syscall_nr).to_string(), None),
    }
}

/// The name of a syscall, in screaming snake case
pub(crate) fn syscall_name(num: Sysnum) -> &'static str {
    match num {
        Sysnum::Send => "SEND",
        Sysnum::Recv => "RECV",
        Sysnum::Reply => "REPLY",
        Sysnum::SetTimer => "SET_TIMER",
        Sysnum::BorrowRead => "BORROW_READ",
        Sysnum::BorrowWrite => "BORROW_WRITE",
        Sysnum::BorrowInfo => "BORROW_INFO",
        Sysnum::IrqControl => "IRQ_CONTROL",
        Sysnum::Panic => "PANIC",
        Sysnum::GetTimer => "GET_TIMER",
        Sysnum::RefreshTaskId => "REFRESH_TASK_ID",
        Sysnum::Post => "POST",
        Sysnum::ReplyFault => "REPLY_FAULT",
        Sysnum::IrqStatus => "IRQ_STATUS",
    }
}
