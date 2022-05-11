// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Implementation of IPC operations on the virtual kernel task.

use abi::{
    FaultInfo, LeaseAttributes, ReplyFaultReason, SchedState, TaskId,
    TaskState, UsageError,
};

use crate::err::UserError;
use crate::task::{current_id, ArchState, NextTask, Task};
use crate::umem::USlice;

/// Message dispatcher.
pub fn handle_kernel_message(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, UserError> {
    // Copy out arguments.
    let args = tasks[caller].save().as_send_args();
    let operation = args.operation();
    // We're not checking these yet as we might not need them.
    let maybe_message = args.message();
    let maybe_response = args.response_buffer();
    drop(args);

    match operation {
        1 => read_task_status(tasks, caller, maybe_message?, maybe_response?),
        2 => restart_task(tasks, caller, maybe_message?),
        3 => fault_task(tasks, caller, maybe_message?),
        4 => read_image_id(tasks, caller, maybe_response?),
        5 => read_task_panic_message(
            tasks,
            caller,
            maybe_message?,
            maybe_response?,
        ),
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

fn read_task_panic_message(
    tasks: &mut [Task],
    caller: usize,
    message: USlice<u8>,
    response: USlice<u8>,
) -> Result<NextTask, UserError> {
    // Extract the index of the patient and check bounds.
    let patient: u32 = deserialize_message(&tasks[caller], message)?;
    let patient = patient as usize;
    if patient >= tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }

    // Is the patient actually waiting at a panic? We'll treat this as
    // recoverable for the supervisor.
    match tasks[patient].state() {
        TaskState::Faulted {
            fault: FaultInfo::Panic,
            ..
        } => {
            // Looks good!
        }
        _ => return Err(UserError::Recoverable(1, NextTask::Same)),
    }

    // Extract the patient's alleged panic buffer. This can fail with a
    // UsageError if the slice is totally bogus (e.g. overlaps the end of the
    // address space). We'll convert this into a return code.
    let panic_msg = tasks[patient]
        .save()
        .as_panic_args()
        .message()
        .map_err(|_| UserError::Recoverable(2, NextTask::Same))?;

    // We don't yet know if `panic_msg` is actually legal in the patient's
    // memory map, but safe_copy below will do that for us.

    // Turning focus to the lease provided by the caller:
    let lease = match crate::syscalls::borrow_lease_generic(tasks, caller, 0, 0)
    {
        Err(UserError::Recoverable(_, wake_hint)) => {
            // "Recoverable" returned from this function means recoverable to
            // the _server,_ but it has already faulted the _client._ So,
            // observe its context switch and move on.
            return Ok(wake_hint);
        }
        Err(UserError::Unrecoverable(fault)) => {
            // "Unrecoverable" assigns fault to the server, generally because it
            // is trying to access leases that aren't defined. However, we're
            // the _kernel,_ and we get to access the lease defined in our ABI
            // if we want to -- if you call this kipc without the right leases,
            // that's on you, not the kernel.
            //
            // And so, we assign fault to the caller.
            return Err(UserError::Unrecoverable(fault));
        }
        Ok(lease) => lease,
    };

    // Does this lease grant write permission? It needs to, since the caller
    // allegedly intends it to receive a panic message.
    if !lease.attributes.contains(LeaseAttributes::WRITE) {
        return Err(UserError::Unrecoverable(FaultInfo::FromServer(
            TaskId::KERNEL,
            ReplyFaultReason::BadLeases,
        )));
    }

    let leased_area = USlice::from(&lease);

    let copy_result =
        crate::umem::safe_copy(tasks, patient, panic_msg, caller, leased_area);
    match copy_result {
        Ok(n) => {
            // Serialize the panic message length as our response message.
            // Do it as a u32 so its size is predictable.
            let msglen = n as u32;
            let response_len =
                serialize_response(&mut tasks[caller], response, &msglen)?;
            tasks[caller]
                .save_mut()
                .set_send_response_and_length(0, response_len);
            Ok(NextTask::Same)
        }
        Err(interact_fault) => {
            // In this case, the "dst" is our caller who wanted to extract the
            // panic message, and the "src" is the patient. If safe_copy
            // detected a problem in "dst," we will fault it. However, if it
            // detected a problem in "src," we're going to _leave src alone_
            // since it's already dead, and convert that into an error code.
            //
            // This is not exactly the logic that's available in the canned
            // InteractFault::apply_to_dst method, because this is a slightly
            // weird situation, so we're going to do this by hand.
            //
            // First, kill the caller if their destination lease wasn't proper.
            if let Some(f) = interact_fault.dst {
                return Err(UserError::Unrecoverable(f));
            }

            // Now, if they're still alive, report an error at the patient if
            // relevant. The safe_copy contract says that either src or dst (or
            // both) is Some, so, we assert that src is some and fail here.
            uassert!(interact_fault.src.is_some());
            return Err(UserError::Recoverable(1, NextTask::Same));
        }
    }
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
