// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Implementation of IPC operations on the virtual kernel task.

use abi::{FaultInfo, Kipcnum, SchedState, TaskState, UsageError};
use core::mem::size_of;
use unwrap_lite::UnwrapLite;

use crate::arch;
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

    match Kipcnum::try_from(args.operation) {
        Ok(Kipcnum::ReadTaskStatus) => {
            read_task_status(tasks, caller, args.message?, args.response?)
        }
        Ok(Kipcnum::ReinitTask) => reinit_task(tasks, caller, args.message?),
        Ok(Kipcnum::FaultTask) => fault_task(tasks, caller, args.message?),
        Ok(Kipcnum::ReadImageId) => {
            read_image_id(tasks, caller, args.response?)
        }
        Ok(Kipcnum::Reset) => reset(tasks, caller, args.message?),
        #[cfg(feature = "dump")]
        Ok(Kipcnum::GetTaskDumpRegion) => {
            get_task_dump_region(tasks, caller, args.message?, args.response?)
        }
        #[cfg(feature = "dump")]
        Ok(Kipcnum::ReadTaskDumpRegion) => {
            read_task_dump_region(tasks, caller, args.message?, args.response?)
        }
        Ok(Kipcnum::SoftwareIrq) => software_irq(tasks, caller, args.message?),
        Ok(Kipcnum::FindFaultedTask) => {
            find_faulted_task(tasks, caller, args.message?, args.response?)
        }

        _ => {
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
            Ok(size_of::<T>())
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

fn reinit_task(
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

#[cfg(feature = "dump")]
fn get_task_dump_region(
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
        Some(abi::TaskDumpRegion {
            base: &tasks[index as usize] as *const _ as u32,
            size: size_of::<Task>() as u32,
        })
    } else {
        tasks[index as usize]
            .region_table()
            .iter()
            .filter(|r| r.dumpable())
            .nth(rindex as usize - 1)
            .map(|r| abi::TaskDumpRegion {
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

#[cfg(feature = "dump")]
fn read_task_dump_region(
    tasks: &mut [Task],
    caller: usize,
    message: USlice<u8>,
    mut response: USlice<u8>,
) -> Result<NextTask, UserError> {
    use crate::umem::safe_copy_dma;
    use crate::util::index2_distinct;

    if caller != 0 {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::NotSupervisor,
        )));
    }

    let (index, region): (u32, abi::TaskDumpRegion) =
        deserialize_message(&tasks[caller], message)?;
    // It's far more convenient to deal with index as a usize from here on.
    let index = index as usize;

    //
    // In addition to assuring that the task index isn't out of range, we do
    // not allow the supervisor to dump itself. This ensures that index !=
    // caller, which makes reasoning about aliasing below significantly easier.
    //
    if index == caller || index >= tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }

    // Get &mut references to the two tasks we're interacting with, which are
    // now ensured to be distinct, so we can treat them as separate entities for
    // borrowing purposes.
    let (caller_task, target_task) = index2_distinct(tasks, caller, index);

    // Convert the region from (caller-controlled) arbitrary numbers into a
    // USlice. This centralizes the check for base+size overflowing, eliminating
    // the need for it below.
    //
    // Note that if the supervisor passes an illegal base+size combination here,
    // we're going to kill the supervisor, implying a reboot. This is the best
    // we can do, since the supervisor is malfunctioning.
    let from =
        USlice::<u8>::from_raw(region.base as usize, region.size as usize)
            .map_err(FaultInfo::SyscallUsage)?;

    //
    // If we are being asked to copy out the target task structure (and only
    // a part of the target task structure), we will copy that directly.  (If
    // this has been somehow malformed, we will fall into the `safe_copy`
    // case, which will fail.)
    //
    let tcb_size = size_of::<Task>();
    let tcb_base = target_task as *mut _ as usize;
    // Because target_task comes from a reference, we can compute the
    // one-past-the-end address for it without overflow (i.e. it is guaranteed
    // not to be up against the top of the address space). So we use a wrapping
    // add here because the compiler can't see that and wants an overflow check.
    let tcb_end = tcb_base.wrapping_add(tcb_size);

    let response_len =
        if from.base_addr() >= tcb_base && from.end_addr() <= tcb_end {
            // Things are about to get weird.
            //
            // We're being asked to copy-out a portion of the raw Task struct
            // representing target_task. This struct is (quite deliberately)
            // _not_ repr(C) or packed, so its layout is essentially undefined,
            // it will contain padding, etc. This means we can't get a view of
            // it as bytes using `zerocopy` -- `zerocopy` will refuse such a
            // type as almost certainly a mistake on our part.
            //
            // Usually, it'd be right to do so, but the audience for the dump is
            // the _debugger,_ and the debugger (by definition) knows the layout
            // of the raw bytes in the type. So, this is one of those rare cases
            // where just reading the raw bytes is correct and good.
            //
            // To avoid potentially aliasing accesses through the `tasks` slice,
            // we're going to read only within the `target_task` reference
            // (which also guards us against any accidental out-of-bounds
            // access).

            // Safety: we're going to read (only read!) the bytes of the Task
            // struct for dumping purposes. Because we derive this authority
            // from the `&mut` `target_task` we know there are no other aliasing
            // references to it. Because we shadow `target_task` here, no code
            // below this can access the original `&mut` until our punning as
            // bytes leaves scope. This requires unsafe because, in general,
            // this is a really bad idea, but in this case
            // - We are casting from a pointer to a type with higher alignment
            //   (Task) to lower alignment (byte array)
            // - We are very comfortable with reading arbitrary undefined data
            //   in padding between fields and the like, because the debugger
            //   will ignore them.
            let target_task: &[u8; size_of::<Task>()] =
                unsafe { core::mem::transmute(target_task) };

            // Now let's grab the requested portion as a sub-slice. This should
            // succeed (see: the comparisons between base+size and region.size
            // above).
            let offset = from.base_addr() - tcb_base;
            let tcb = &target_task[offset..from.len()];

            let to = caller_task
                .try_write(&mut response)
                .map_err(UserError::Unrecoverable)?;
            let copy_len = to.len().min(tcb.len());
            to[..copy_len].copy_from_slice(&tcb[..copy_len]);
            copy_len
        } else {
            //
            // We have memory that is not completely contained by the target
            // task structure -- either because it is in the task's memory
            // or because it is an invalid address/length (e.g., a part of
            // the task structure that also overlaps with other kernel
            // memory, or wholly bogus).  In all of these cases, we will
            // rely on `safe_copy_dma` to do the validation.
            //
            // Note that this applies to: attempts to read out of bounds of the
            // target task's TCB, attempts to read other tasks' TCBs, etc. So
            // this is effectively serving as the validation backstop for the
            // TCB access code above.
            //

            match safe_copy_dma(tasks, index, from, caller, response) {
                Err(interact) => {
                    // So, this is a weird case. We are attempting to transfer
                    // memory from one task to another, but unlike every other
                    // use of safe_copy in the kernel, the address ranges on
                    // _both sides_ are controlled by one party (the
                    // supervisor).
                    //
                    // This means the InteractFault produced by `safe_copy` is
                    // shaped slightly wrong for the task. Counter-intuitively,
                    // the supervisor (meaning `caller`) should be blamed for
                    // faults on _both_ the source and destination side.
                    //
                    // So we do this using the following code, which looks wrong
                    // at first glance since it's backwards from every other
                    // routine in the kernel -- but its shape is deliberate! We
                    // really do want to apply the SRC fault to the caller,
                    // followed by any remaining DST fault (by returning it from
                    // this function, which means it'll be applied by the
                    // generic syscall error handler).
                    //
                    // Note that if the supervisor is _really_ misbehaving, this
                    // can result in the delivery of two faults to it (the SRC
                    // and DST faults). The behavior of `task::force_fault` in
                    // this case is well-defined (the last one wins) so this is
                    // ok.
                    //
                    // The wake hint will only be used if there is a SRC fault
                    // and no DST fault.
                    return Ok(interact.apply_to_src(tasks, caller)?);
                }
                Ok(len) => len,
            }
        };

    tasks[caller]
        .save_mut()
        .set_send_response_and_length(0, response_len);
    Ok(NextTask::Same)
}

fn software_irq(
    tasks: &mut [Task],
    caller: usize,
    message: USlice<u8>,
) -> Result<NextTask, UserError> {
    if caller != 0 {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::NotSupervisor,
        )));
    }

    let (index, notification): (u32, u32) =
        deserialize_message(&tasks[caller], message)?;

    if index as usize >= tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }

    // Look up which IRQs are mapped to the target task.
    let irqs = crate::startup::HUBRIS_TASK_IRQ_LOOKUP
        .get(abi::InterruptOwner {
            task: index,
            notification,
        })
        .ok_or(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::NoIrq,
        )))?;

    for &irq in irqs.iter() {
        // Any error here would be a problem in our dispatch table, not the
        // caller, so we panic because we want to hear about it.
        crate::arch::pend_software_irq(irq).unwrap_lite();
    }

    tasks[caller].save_mut().set_send_response_and_length(0, 0);
    Ok(NextTask::Same)
}

fn find_faulted_task(
    tasks: &mut [Task],
    caller: usize,
    message: USlice<u8>,
    response: USlice<u8>,
) -> Result<NextTask, UserError> {
    if caller != 0 {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::NotSupervisor,
        )));
    }

    let index = deserialize_message::<u32>(&tasks[caller], message)? as usize;

    // Note: we explicitly permit index == tasks.len(), which causes us to wrap
    // and end the search.
    if index > tasks.len() {
        return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )));
    }
    let i = tasks[index..]
        .iter()
        .position(|task| matches!(task.state(), TaskState::Faulted { .. }))
        .map(|i| i + index)
        .unwrap_or(0);

    let response_len =
        serialize_response(&mut tasks[caller], response, &(i as u32))?;
    tasks[caller]
        .save_mut()
        .set_send_response_and_length(0, response_len);
    Ok(NextTask::Same)
}
