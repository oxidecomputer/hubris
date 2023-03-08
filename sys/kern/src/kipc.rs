// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Implementation of IPC operations on the virtual kernel task.

use abi::{FaultInfo, Kipcnum, SchedState, TaskState, UsageError};

use crate::arch;
use crate::err::UserError;
use crate::task::{current_id, ArchState, NextTask, Task};
use crate::umem::USlice;
use core::convert::TryFrom;

/// Message dispatcher.
pub fn handle_kernel_message(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, UserError> {
    // Copy out arguments.
    let args = tasks[caller].save().as_send_args();

    match Kipcnum::try_from(args.operation) {
        Ok(Kipcnum::ReadTaskStatus) => {
            read_task_status(tasks, caller, args.message?, args.response?)
        }
        Ok(Kipcnum::RestartTask) => restart_task(tasks, caller, args.message?),
        Ok(Kipcnum::FaultTask) => fault_task(tasks, caller, args.message?),
        Ok(Kipcnum::ReadImageId) => {
            read_image_id(tasks, caller, args.response?)
        }
        Ok(Kipcnum::Reset) => reset(tasks, caller, args.message?),
        Ok(Kipcnum::ReadCaboosePos) => {
            read_caboose_pos(tasks, caller, args.response?)
        }
        Ok(Kipcnum::ReadTaskDumpRegion) => {
            read_task_dump_region(tasks, caller, args.message?, args.response?)
        }
        Err(_) => {
            // Task has sent an unknown message to the kernel. That's bad.
            Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
                UsageError::BadKernelMessage,
            )))
        }
    }
}
fn reset(_tasks: &mut [Task], _caller: usize, _message: USlice<u8>) -> ! {
    arch::reset()
}

fn deserialize_message<T>(
    task: &Task,
    message: USlice<u8>,
) -> Result<T, UserError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let (msg, _) = ssmarshal::deserialize(task.try_read(&message)?)
        .map_err(|_| UsageError::BadKernelMessage)?;
    Ok(msg)
}

fn serialize_response<T>(
    task: &mut Task,
    mut buf: USlice<u8>,
    val: &T,
) -> Result<usize, UserError>
where
    T: serde::Serialize,
{
    match ssmarshal::serialize(task.try_write(&mut buf)?, val) {
        Ok(size) => Ok(size),
        Err(ssmarshal::Error::EndOfStream) => {
            // The client provided a response buffer that is too small. We
            // actually tolerate this, and report back the size of a buffer that
            // *would have* worked. It's up to the caller to notice.
            Ok(core::mem::size_of::<T>())
        }
        Err(_) => Err(UsageError::BadKernelMessage.into()),
    }
}

fn read_task_status(
    tasks: &mut [Task],
    caller: usize,
    message: USlice<u8>,
    response: USlice<u8>,
) -> Result<NextTask, UserError> {
    let index: u32 = deserialize_message(&tasks[caller], message)?;
    if index as usize >= tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }
    // cache other state before taking out a mutable borrow on tasks
    let other_state = *tasks[index as usize].state();

    let response_len =
        serialize_response(&mut tasks[caller], response, &other_state)?;
    tasks[caller]
        .save_mut()
        .set_send_response_and_length(0, response_len);
    Ok(NextTask::Same)
}

fn read_task_dump_region(
    tasks: &mut [Task],
    caller: usize,
    message: USlice<u8>,
    response: USlice<u8>,
) -> Result<NextTask, UserError> {
    let (index, rindex): (u32, u32) =
        deserialize_message(&tasks[caller], message)?;
    if index as usize >= tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }

    let rval = if rindex == 0 {
        let size: u32 = core::mem::size_of::<Task>() as u32;

        Some(abi::TaskDumpRegion {
            base: tasks.as_ptr() as u32 + index * size,
            size: size,
        })
    } else {
        tasks[index as usize]
            .region_table()
            .iter()
            .filter(|r| r.dumpable())
            .enumerate()
            .find(|(ndx, _)| *ndx + 1 == rindex as usize)
            .map(|(_, r)| abi::TaskDumpRegion {
                base: r.base,
                size: r.size,
            })
    };

    let response_len = serialize_response(&mut tasks[caller], response, &rval)?;
    tasks[caller]
        .save_mut()
        .set_send_response_and_length(0, response_len);
    Ok(NextTask::Same)
}

fn restart_task(
    tasks: &mut [Task],
    caller: usize,
    message: USlice<u8>,
) -> Result<NextTask, UserError> {
    let (index, start): (u32, bool) =
        deserialize_message(&tasks[caller], message)?;
    let index = index as usize;
    if index >= tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }
    let old_id = current_id(tasks, index);
    tasks[index].reinitialize();
    if start {
        tasks[index].set_healthy_state(SchedState::Runnable);
    }

    // Restarting a task can have implications for other tasks. We don't want to
    // leave tasks sitting around waiting for a reply that will never come, for
    // example. So, make a pass over the task table and unblock anyone who was
    // expecting useful work from the now-defunct task.
    for (i, task) in tasks.iter_mut().enumerate() {
        // Just to make this a little easier to think about, don't check either
        // of the tasks involved in the restart operation. Neither should be
        // affected anyway.
        if i == caller || i == index {
            continue;
        }

        // We'll skip processing faulted tasks, because we don't want to lose
        // information in their fault records by changing their states.
        if let TaskState::Healthy(sched) = task.state() {
            match sched {
                SchedState::InRecv(Some(peer))
                | SchedState::InSend(peer)
                | SchedState::InReply(peer)
                    if peer == &old_id =>
                {
                    // Please accept our sincere condolences on behalf of the
                    // kernel.
                    let code = abi::dead_response_code(peer.generation());

                    task.save_mut().set_error_response(code);
                    task.set_healthy_state(SchedState::Runnable);
                }
                _ => (),
            }
        }
    }

    if index == caller {
        // Welp, they've restarted themselves. Best not return anything then.
        if !start {
            // And they have asked not to be started, so we can't even fast-path
            // return to their task!
            return Ok(NextTask::Other);
        }
    } else {
        tasks[caller].save_mut().set_send_response_and_length(0, 0);
    }
    Ok(NextTask::Same)
}

///
/// Inject a fault into a specified task.  The injected fault will be of a
/// distinct type (`FaultInfo::Injected`) and will contain as a payload the
/// task that injected the fault.  As with restarting, we allow any task to
/// inject a fault into any other task but -- unlike restarting -- we
/// (1) explicitly forbid any fault injection into the supervisor and
/// (2) explicitly forbid any fault injection into the current task (for
/// which the caller should be instead explicitly panicking).
///
fn fault_task(
    tasks: &mut [Task],
    caller: usize,
    message: USlice<u8>,
) -> Result<NextTask, UserError> {
    let index: u32 = deserialize_message(&tasks[caller], message)?;
    let index = index as usize;

    if index == 0 || index == caller {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::IllegalTask,
        )));
    }

    if index >= tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }

    let id = current_id(tasks, caller);
    let _ = crate::task::force_fault(tasks, index, FaultInfo::Injected(id));
    tasks[caller].save_mut().set_send_response_and_length(0, 0);

    Ok(NextTask::Same)
}

fn read_image_id(
    tasks: &mut [Task],
    caller: usize,
    response: USlice<u8>,
) -> Result<NextTask, UserError> {
    let id =
        unsafe { core::ptr::read_volatile(&crate::startup::HUBRIS_IMAGE_ID) };
    let response_len = serialize_response(&mut tasks[caller], response, &id)?;
    tasks[caller]
        .save_mut()
        .set_send_response_and_length(0, response_len);
    Ok(NextTask::Same)
}

fn read_caboose_pos(
    tasks: &mut [Task],
    caller: usize,
    response: USlice<u8>,
) -> Result<NextTask, UserError> {
    // SAFETY: populated by the linker + build system
    let header = unsafe { &crate::header::HEADER };

    // The end-of-image position is given as an image length, so we need to
    // apply it as an offset to the start-of-image.
    extern "C" {
        static __start_vector: [u32; 0];
    }
    // SAFETY: populated by the linker script
    let image_start = unsafe { &__start_vector } as *const u32 as u32;
    let image_end = image_start + header.total_image_len;

    // The caboose records its own size in its last word.  The recorded size is
    // **inclusive** of this word.
    //
    // SAFETY: populated by the build system to a valid value
    let caboose_size: u32 =
        unsafe { core::ptr::read_volatile((image_end - 4) as *const u32) };

    // Calculate the beginning of the caboose.  If the caboose is unpopulated,
    // then we expect a random value (or 0xFFFFFFFF) as its size, which we can
    // catch because it will give us an obviously invalid start location.
    let caboose_start = image_end.saturating_sub(caboose_size);
    let out = if caboose_start <= image_start {
        (0, 0)
    } else {
        // SAFETY: we know this pointer is within the image flash region
        let v =
            unsafe { core::ptr::read_volatile(caboose_start as *const u32) };
        if v == abi::CABOOSE_MAGIC {
            (caboose_start + 4, image_end - 4)
        } else {
            (0, 0)
        }
    };

    let response_len = serialize_response(&mut tasks[caller], response, &out)?;
    tasks[caller]
        .save_mut()
        .set_send_response_and_length(0, response_len);
    Ok(NextTask::Same)
}
