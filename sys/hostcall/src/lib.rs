// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Syscall transport for Hubris tasks running as host processes.
//!
//! When a task is compiled for the host instead of for a Cortex-M target, it
//! has no kernel to trap into. Instead, every syscall becomes a blocking
//! request/response exchange with a *fixture* process that plays the role of
//! the kernel and of every other task in the system. The task is the client
//! and the fixture is the server.
//!
//! This crate defines that exchange:
//!
//! * [`syscalls`] and [`runtime`] are the [`nprpc`] interfaces: one method per
//!   syscall, plus the few things a task needs from its environment that the
//!   real kernel resolves at link time (task slots).
//! * The request and response types mirror the *register-level* syscall ABI:
//!   task ids are `u16`; codes, masks, lengths and indices are `u32` (the
//!   kernel receives them in 32-bit registers, and `usize` has no portable
//!   schema); timestamps are `u64`; buffers travel as byte vectors. Nothing
//!   here depends on the `abi` crate, so the fixture side is free to be a
//!   plain host program.
//! * [`frame`] is the COBS framing used to delimit messages on a byte stream.
//! * [`Client`] and [`Server`] are ready-made [`nprpc`] backends over any
//!   reader/writer pair; the task uses them over its own stdin and stdout, the
//!   fixture over the child's stdin and stdout.
//!
//! Syscalls are blocking in Hubris, and the task process is single-threaded,
//! so a strictly ordered request/response protocol loses nothing.

pub mod frame;
mod io;

use nprpc::{compose_interfaces, interface};
use postcard_schema_ng::Schema;
use serde::{Deserialize, Serialize};

pub use io::{Client, HeapStorage, IoError, Server, StreamIo};
/// The RPC crate the backends are built on, re-exported so users can name its
/// `Response` and error types without depending on it themselves.
pub use nprpc;

/// Task id the kernel uses for itself; notifications arrive from it.
pub const KERNEL_TASK_ID: u16 = !0;

/// Exit status of the task process after a `panic` request was acknowledged.
pub const EXIT_PANIC: i32 = 101;
/// Exit status of the task process after the fixture answered with a
/// [`Fault`].
pub const EXIT_FAULT: i32 = 102;
/// Exit status of the task process when the transport to the fixture failed.
pub const EXIT_TRANSPORT: i32 = 103;

/// The fixture's way of killing a task, as the kernel would for a syscall it
/// considers illegal (bad task id, lease out of range, ...).
///
/// On receiving this the task process prints the description and exits with
/// [`EXIT_FAULT`] instead of returning from the syscall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct Fault {
    pub description: String,
}

/// Result of a syscall as decided by the fixture.
pub type Outcome<T> = Result<T, Fault>;

/// One entry of the lease table attached to a SEND.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct Lease {
    /// `abi::LeaseAttributes` bits.
    pub attributes: u32,
    /// Length of the leased memory in bytes.
    pub len: u32,
    /// The leased bytes, present only if the lease is readable; empty
    /// otherwise.
    pub contents: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct SendRequest {
    pub target: u16,
    pub operation: u16,
    pub message: Vec<u8>,
    /// Size of the buffer the task offered for the reply.
    pub reply_capacity: u32,
    pub leases: Vec<Lease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct SendResponse {
    /// Response code; `0` for success, `abi::dead_response_code` when the
    /// peer is dead, otherwise application-defined.
    pub code: u32,
    /// Reply bytes. Must not exceed the request's `reply_capacity`.
    pub reply: Vec<u8>,
    /// New contents for each lease, in lease-table order: `None` leaves the
    /// task's memory untouched; `Some` must be no longer than the lease and
    /// is written back starting at offset 0.
    pub lease_writebacks: Vec<Option<Vec<u8>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct RecvRequest {
    /// Size of the buffer the task offered for the incoming message.
    pub capacity: u32,
    pub notification_mask: u32,
    /// `Some` for a closed receive.
    pub specific_sender: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct RecvMessage {
    /// Sending task, or [`KERNEL_TASK_ID`] for a notification.
    pub sender: u16,
    /// Operation code, or the notification bits when `sender` is the kernel.
    pub operation: u32,
    /// The full message. As the kernel does, the task truncates it to its
    /// buffer but reports the full length.
    pub message: Vec<u8>,
    pub response_capacity: u32,
    pub lease_count: u32,
}

/// `Err` carries the dead-peer code of a failed closed receive.
pub type RecvResponse = Result<RecvMessage, u32>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct ReplyRequest {
    pub peer: u16,
    pub code: u32,
    pub message: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct SetTimerRequest {
    pub deadline: Option<u64>,
    pub notifications: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct TimerState {
    pub now: u64,
    pub deadline: Option<u64>,
    pub on_deadline: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct BorrowReadRequest {
    pub lender: u16,
    pub index: u32,
    pub offset: u32,
    /// Size of the task's destination buffer.
    pub len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct BorrowReadResponse {
    pub code: u32,
    /// Bytes read; must not exceed the request's `len`.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct BorrowWriteRequest {
    pub lender: u16,
    pub index: u32,
    pub offset: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct BorrowWriteResponse {
    pub code: u32,
    /// Number of bytes actually written.
    pub len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct BorrowInfoRequest {
    pub lender: u16,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct BorrowInfo {
    /// `abi::LeaseAttributes` bits.
    pub attributes: u32,
    pub len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct IrqControlRequest {
    pub mask: u32,
    /// `abi::IrqControlArg` bits.
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct PostRequest {
    pub task: u16,
    pub bits: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct ReplyFaultRequest {
    pub peer: u16,
    /// `abi::ReplyFaultReason` as passed in the syscall register.
    pub reason: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct PanicRequest {
    /// The panic message. Usually UTF-8, but the task may have truncated it
    /// mid-character, exactly as on the real target.
    pub message: Vec<u8>,
}

interface! {
    /// One method per kernel syscall, in `abi::Sysnum` order.
    ///
    /// Every method except `panic` returns an [`Outcome`], so the fixture can
    /// answer with a [`Fault`] wherever the kernel would kill the task.
    mod syscalls {
        fn send(SendRequest) -> Outcome<SendResponse>;
        fn recv(RecvRequest) -> Outcome<RecvResponse>;
        fn reply(ReplyRequest) -> Outcome<()>;
        fn set_timer(SetTimerRequest) -> Outcome<()>;
        fn borrow_read(BorrowReadRequest) -> Outcome<BorrowReadResponse>;
        fn borrow_write(BorrowWriteRequest) -> Outcome<BorrowWriteResponse>;
        fn borrow_info(BorrowInfoRequest) -> Outcome<Option<BorrowInfo>>;
        fn irq_control(IrqControlRequest) -> Outcome<()>;
        /// The task exits with [`EXIT_PANIC`] once this is acknowledged.
        fn panic(PanicRequest) -> ();
        fn get_timer(()) -> Outcome<TimerState>;
        fn refresh_task_id(u16) -> Outcome<u16>;
        fn post(PostRequest) -> Outcome<u32>;
        fn reply_fault(ReplyFaultRequest) -> Outcome<()>;
        fn irq_status(u32) -> Outcome<u32>;
    }
}

interface! {
    /// Things the build system resolves for a real task, which a host task
    /// must ask the fixture for instead.
    mod runtime {
        /// Resolves a `task_slot!` name to a task index.
        fn task_slot(String) -> Outcome<u16>;
    }
}

compose_interfaces! {
    mod: all,
    interfaces: [
        crate::syscalls,
        crate::runtime,
    ]
}
