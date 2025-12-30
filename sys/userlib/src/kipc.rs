// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Operations implemented by IPC with the kernel task.
//!
//! # On checking return values
//!
//! All the functions in this module send IPCs to the kernel directly. It's not
//! generally useful for us to check the return codes, except in cases where the
//! IPC is defined as able to fail. We have no choice but to trust the kernel,
//! since it controls everything.
//!
//! As a result, asserting on return codes and lengths when they can only be zero just
//! wastes flash space in the supervisor.

use core::num::NonZeroUsize;

use abi::{Kipcnum, ReadPanicMessageError, TaskId};
use zerocopy::IntoBytes;

use crate::{sys_send, UnwrapLite, PANIC_MESSAGE_MAX_LEN};

pub fn read_task_status(task: usize) -> abi::TaskState {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let mut response = [0; core::mem::size_of::<abi::TaskState>()];
    let (_rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReadTaskStatus as u16,
        task.as_bytes(),
        &mut response,
        &[],
    );
    ssmarshal::deserialize(&response[..len]).unwrap_lite().0
}

/// Scans forward from index `task` looking for a task in faulted state.
///
/// If no tasks at `task` or greater indices are faulted, this returns `None`.
///
/// If a faulted task at index `i` is found, returns `Some(i)`.
///
/// `task` may equal the number of tasks in the system (i.e. a one-past-the-end
/// index). In that case, this returns `None` every time. Larger values will get
/// you killed.
///
/// The return value is a `NonZeroUsize` because this can't ever return zero,
/// since that would mean the supervisor (presumably the caller of this
/// function!) is in faulted state.
pub fn find_faulted_task(task: usize) -> Option<NonZeroUsize> {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let mut response = 0_u32;
    let (_, _) = sys_send(
        TaskId::KERNEL,
        Kipcnum::FindFaultedTask as u16,
        task.as_bytes(),
        response.as_mut_bytes(),
        &[],
    );
    NonZeroUsize::new(response as usize)
}

pub fn get_task_dump_region(
    task: usize,
    region: usize,
) -> Option<abi::TaskDumpRegion> {
    let msg = (task as u32, region as u32);
    let mut buf = [0; core::mem::size_of::<(u32, u32)>()];
    ssmarshal::serialize(&mut buf, &msg).unwrap_lite();

    let mut response = [0; core::mem::size_of::<Option<abi::TaskDumpRegion>>()];
    let (_rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::GetTaskDumpRegion as u16,
        &buf,
        &mut response,
        &[],
    );
    ssmarshal::deserialize(&response[..len]).unwrap_lite().0
}

pub fn read_task_dump_region(
    task: usize,
    region: abi::TaskDumpRegion,
    response: &mut [u8],
) -> usize {
    let msg = (task as u32, region);
    let mut buf = [0; core::mem::size_of::<(u32, abi::TaskDumpRegion)>()];
    ssmarshal::serialize(&mut buf, &msg).unwrap_lite();

    let (_rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReadTaskDumpRegion as u16,
        &buf,
        response,
        &[],
    );
    len
}

pub fn reinit_task(task: usize, start: bool) {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let msg = (task as u32, start);
    let mut buf = [0; core::mem::size_of::<(u32, bool)>()];
    ssmarshal::serialize(&mut buf, &msg).unwrap_lite();
    let (_rc, _len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReinitTask as u16,
        &buf,
        &mut [],
        &[],
    );
}

pub fn fault_task(task: usize) {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let (_rc, _len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::FaultTask as u16,
        task.as_bytes(),
        &mut [],
        &[],
    );
}

pub fn system_restart() -> ! {
    let _ = sys_send(TaskId::KERNEL, Kipcnum::Reset as u16, &[], &mut [], &[]);
    loop {
        core::sync::atomic::compiler_fence(
            core::sync::atomic::Ordering::SeqCst,
        );
    }
}

pub fn read_image_id() -> u64 {
    let mut response = [0; core::mem::size_of::<u64>()];
    let (_rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReadImageId as u16,
        &[],
        &mut response,
        &[],
    );
    ssmarshal::deserialize(&response[..len]).unwrap_lite().0
}

/// Trigger the interrupt(s) mapped to the given task's notification mask.
pub fn software_irq(task: usize, mask: u32) {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let msg = (task as u32, mask);
    let mut buf = [0; core::mem::size_of::<(u32, u32)>()];
    ssmarshal::serialize(&mut buf, &msg).unwrap_lite();

    let (_rc, _len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::SoftwareIrq as u16,
        &buf,
        &mut [],
        &[],
    );
}

/// Reads a task's panic message into the provided `buf`, if the task is
/// panicked.
///
/// Note that Hubris normally only preserves the first [`PANIC_MESSAGE_MAX_LEN`] bytes of
/// a task's panic message, and panic messages greater than that length are
/// truncated. Thus, this function accepts a buffer of that length.
///
/// # Returns
///
/// - [`Ok`]`(&[u8])` if the task is panicked. The returned slice is borrowed
///   from `buf`, and contains the task's panic message as a sequence of
///   UTF-8 bytes. Note that the slice may be empty, if the task has panicked
///   but was compiled without panic messages enabled.
/// - [`Err`]`(`[`ReadPanicMessageError::TaskNotPanicked`]`)` if the task is
///   not currently faulted due to a panic.
/// - [`Err`]`(`[`ReadPanicMessageError::BadPanicMessage`]`)` if the task has
///   panicked but the panic message buffer is invalid to read from.
pub fn read_panic_message(
    task: usize,
    buf: &mut [u8; PANIC_MESSAGE_MAX_LEN],
) -> Result<&[u8], ReadPanicMessageError> {
    let task = task as u32;
    let (rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReadPanicMessage as u16,
        task.as_bytes(),
        &mut buf[..],
        &[],
    );

    if rc == 0 {
        Ok(&buf[..len])
    } else {
        // If the kernel sent us an unknown response code....i dunno, guess i'll die?
        Err(ReadPanicMessageError::try_from(rc).unwrap_lite())
    }
}
