//! Implementation of IPC operations on the virtual kernel task.

use abi::{FaultInfo, FaultSource, SchedState, TaskState, UsageError, DEAD};

use crate::err::UserError;
use crate::task::{current_id, ArchState, NextTask, Task};
use crate::umem::USlice;

/// Message dispatcher.
pub fn handle_kernel_message(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, UserError> {
    // Copy out arguments.
    let args = tasks[caller].save.as_send_args();
    let operation = args.operation();
    // We're not checking these yet as we might not need them.
    let maybe_message = args.message();
    let maybe_response = args.response_buffer();
    drop(args);

    match operation {
        1 => read_task_status(tasks, caller, maybe_message?, maybe_response?),
        2 => restart_task(tasks, caller, maybe_message?),
        _ => {
            // Task has sent an unknown message to the kernel. That's bad.
            return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
                UsageError::BadKernelMessage,
            )));
        }
    }
}

fn deserialize_message<T>(
    task: &Task,
    message: USlice<u8>,
) -> Result<T, UserError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    if !task.can_read(&message) {
        return Err(UserError::Unrecoverable(FaultInfo::MemoryAccess {
            address: Some(message.base_addr() as u32),
            source: FaultSource::Kernel,
        }));
    }
    let (msg, _) = ssmarshal::deserialize(unsafe { message.assume_readable() })
        .map_err(|_| UsageError::BadKernelMessage)?;
    Ok(msg)
}

fn serialize_response<T>(
    task: &Task,
    mut buf: USlice<u8>,
    val: &T,
) -> Result<usize, UserError>
where
    T: serde::Serialize,
{
    if !task.can_write(&buf) {
        return Err(UserError::Unrecoverable(FaultInfo::MemoryAccess {
            address: Some(buf.base_addr() as u32),
            source: FaultSource::Kernel,
        }));
    }
    match ssmarshal::serialize(unsafe { buf.assume_writable() }, val) {
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
    if index as usize > tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }
    let response_len = serialize_response(
        &tasks[caller],
        response,
        &tasks[index as usize].state,
    )?;
    tasks[caller]
        .save
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
    if index > tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }
    let old_id = current_id(tasks, index);
    tasks[index].reinitialize();
    if start {
        tasks[index].state = TaskState::Healthy(SchedState::Runnable);
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
        if let TaskState::Healthy(sched) = task.state {
            match sched {
                SchedState::InRecv(Some(peer))
                | SchedState::InSend(peer)
                | SchedState::InReply(peer)
                    if peer == old_id =>
                {
                    // Please accept our sincere condolences on behalf of the
                    // kernel.
                    task.save.set_error_response(DEAD);
                    task.state = TaskState::Healthy(SchedState::Runnable);
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
        tasks[caller].save.set_send_response_and_length(0, 0);
    }
    Ok(NextTask::Same)
}
