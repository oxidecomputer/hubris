// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Syscall transport for Hubris tasks running as host processes.
//!
//! As part of [RFD#753], the `hostcall` crate implements the wire protocol
//! between a host-native kernel and host-native userlib. The host-native kernel
//! spawns each host-native task as a native OS process, and communication is
//! handled via blocking request/response pairs over stdout and stdin.
//!
//! ## Wire protocol for syscalls
//!
//! COBS encoding is used for framing, and messages are serialized using the
//! `postcard` crate.
//!
//! There are two main top level types (which serve as the wire format) in this
//! crate:
//!
//! 1. The [`Syscall`] type, which is emitted by the `userlib` implementation
//!    when the Task performs a system call
//! 2. The [`Return`] type, which is emitted by the `kern` implementation when
//!    the kernel resumes the task
//!
//! The general control flow of users of this crate is roughly:
//!
//! 1. The host-native kernel spawns the task.
//! 2. The kernel immediately begins waiting for a [`Syscall`] message from the
//!    task using `read_message::<Syscall>()`.
//! 3. The task begins operation, until it is ready to make its first system
//!    call, at which point it makes the system call by using
//!    `write_message::<Syscall>()`. The task immediately blocks, waiting for
//!    a response with `read_message::<Return>()`.
//! 4. The kernel obtains the `Syscall` and processes it, potentially preparing
//!    a `Return` for the task.
//! 5. The kernel performs scheduling decisions, and decides which task to
//!    resume into. This MAY not be the task that just provided the `Syscall`.
//! 6. At some (later) point, the kernel decides to resume the given task, and
//!    emits the response using `write_message::<Return>()`.
//! 7. We return to step 2, and the process repeats.
//!
//! This process achieves the lock-step operation of a "real" hubris kernel, as
//! there is one actor working at any given time, and all other entities are
//! blocked in a `read_message()` call, despite being separate processes.
//!
//! Our separate processes are not expected to have any shared resources,
//! including shared memory or address space. For this reason, the [`Syscall`]
//! and [`Return`] types will deep-copy any contents included or referenced
//! during the syscall/response process. Each side "fixes up" the request to
//! reference the received deep-copy contents to refer to their local copies.
//!
//! In the kernel, this means that we look at the contents and leases from the
//! received `Syscall`, and on the userlib side, this means that return values
//! received in the `Return` are used to overwrite the task-provided buffers or
//! leases.
//!
//! ## Task slot IDs
//!
//! In the regular build process, the mapping of task names to task IDs is
//! calculated by `xtask`, and inserted into the resulting task binaries.
//!
//! In the host-native environment, the kernel is expected to spawn the task
//! process with the task ID mappings stored in the `HUBRIS_TASK_SLOTS`
//! environment variable, using a comma-separated `NAME=INDEX` format.
//!
//! [RFD#753]: https://rfd.shared.oxide.computer/rfd/0753

use std::io::{BufRead, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Task id the kernel uses for itself; notifications arrive from it.
pub const KERNEL_TASK_ID: u16 = abi::TaskId::KERNEL.0;

/// Exit status of the task process after it reported a panic.
pub const EXIT_PANIC: i32 = 101;
/// Exit status of the task process when the transport to the fixture failed.
pub const EXIT_TRANSPORT: i32 = 103;

/// One entry of the lease table attached to a SEND.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// Lease attributes
    pub attributes: abi::LeaseAttributes,
    /// Length of the leased memory in bytes.
    pub len: u32,
    /// The leased bytes, present only if the lease is readable; empty
    /// otherwise.
    pub contents: Vec<u8>,
}

/// A syscall request from `userlib` to `kern`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Syscall {
    /// The syscall number
    pub nr: abi::Sysnum,
    /// The argument registers, as the syscall ABI defines them for `nr`,
    /// with zero wherever it calls for an address.
    ///
    /// In the future, we may want these arguments to be native-word sized for
    /// portability reasons, but for now we only support emulation of 32-bit
    /// targets, so each "register argument" is only a `u32` value.
    pub args: [u32; 7],
    /// The bytes the syscall passes in (typically as an inptr + len pair of
    /// argument values), e.g. the message of a SEND or a REPLY, the data of a
    /// BORROW_WRITE, or the message of a PANIC.
    ///
    /// Empty for all other system calls.
    pub input: Vec<u8>,
    /// Max size of the buffer the task offers for bytes coming back (typically
    /// as an outptr + len pair of argument values), e.g. the reply to a SEND,
    /// the message of a RECV, the data of a BORROW_READ.
    ///
    /// Zero for the others.
    ///
    /// Same note regarding `u32` vs `usize` as in `Self::args`.
    pub output_len: u32,
    /// The lease table included as part of a SEND request, empty for all
    /// other system calls.
    pub leases: Vec<Lease>,
}

/// A syscall response from `kern` to `userlib`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Return {
    /// The return registers, as the syscall ABI defines them for the
    /// syscall.
    ///
    /// Same note regarding `u32` vs `usize` as in `Syscall.args`.
    pub rets: [u32; 6],
    /// Contents for the buffer the task offered, starting at its beginning.
    /// May be shorter than [`Syscall::output_len`] but not longer.
    pub output: Vec<u8>,
    /// New contents for each lease, in lease-table order.
    ///
    /// `None` when the task's memory was untouched by the kernel.
    /// `Some` when the kernel wrote to this lease. Must be not be longer than
    /// the lease, and is written back starting at offset 0.
    pub lease_writebacks: Vec<Option<Vec<u8>>>,
}

/// Transport-level errors.
#[derive(Debug)]
pub enum IoError {
    Io(std::io::Error),
    /// The stream ended in the middle of a message.
    Eof,
    /// A message did not decode as COBS or as the expected type.
    Decode,
}

impl From<std::io::Error> for IoError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Eof => write!(f, "the stream ended inside a message"),
            Self::Decode => write!(f, "received a message that is not valid"),
        }
    }
}

impl std::error::Error for IoError {}

/// Writes one message (blocking), flushing immediately.
pub fn write_message<T: Serialize>(
    writer: &mut impl Write,
    message: &T,
) -> Result<(), IoError> {
    let raw = postcard::to_stdvec(message)
        .expect("serializing to a vector cannot fail");
    let mut encoded = cobs::encode_vec(&raw);
    encoded.push(0);
    writer.write_all(&encoded)?;
    writer.flush()?;
    Ok(())
}

/// Reads one message (blocking).
///
/// Returns `Ok(None)` on a clean end of stream to signal that the peer has
/// hung up.
pub fn read_message<T: DeserializeOwned>(
    reader: &mut impl BufRead,
) -> Result<Option<T>, IoError> {
    let mut encoded = Vec::new();
    if reader.read_until(0, &mut encoded)? == 0 {
        return Ok(None);
    }
    let Some(0) = encoded.pop() else {
        return Err(IoError::Eof);
    };
    let raw = cobs::decode_vec(&encoded).map_err(|_| IoError::Decode)?;
    postcard::from_bytes(&raw)
        .map(Some)
        .map_err(|_| IoError::Decode)
}

/// Environment variable through which a task process is told its task
/// slots: a comma-separated list of `NAME=INDEX`, as [`task_slots_value`]
/// makes it. On the real targets the build patches these into the binary
/// after linking.
pub const TASK_SLOTS_VAR: &str = "HUBRIS_TASK_SLOTS";

/// The value of [`TASK_SLOTS_VAR`] for the given slots.
pub fn task_slots_value<'a>(
    slots: impl IntoIterator<Item = (&'a str, u16)>,
) -> String {
    slots
        .into_iter()
        .map(|(name, index)| format!("{name}={index}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Looks a task slot up in this process's [`TASK_SLOTS_VAR`].
pub fn task_slot(name: &str) -> Option<u16> {
    std::env::var(TASK_SLOTS_VAR)
        .ok()?
        .split(',')
        .filter_map(|entry| entry.split_once('='))
        .find(|(n, _)| *n == name)
        .and_then(|(_, index)| index.parse().ok())
}
