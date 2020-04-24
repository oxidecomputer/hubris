//! Hubris kernel model.
//!
//! This code is intended to lay out the design concepts for the Hubris kernel
//! implementation and make some points about algorithm implementation. It may
//! evolve to become the actual kernel, or it may not.
//!
//! Currently, this is intended to be portable to both ARM and x86, for testing
//! and simulation purposes.
//!
//! # Algorithm Naivety Principles
//!
//! This implementation uses *really naive algorithms*. This is deliberate. The
//! intent is:
//!
//! 1. To use safe Rust for as much as possible.
//! 2. To use easily understood and debugged algorithms.
//! 3. To revisit these decisions if they become performance problems.
//!
//! Assumptions enabling our naivete:
//!
//! - The total number of tasks is fixed (in a given build) and small. Say, less
//!   than 200.
//! - We are not attempting to achieve predictably low worst-case execution
//!   bounds or any realtime nonsense like that.

#![cfg_attr(target_os = "none", no_std)]
#![feature(asm)]
#![feature(naked_functions)]

pub mod app;
pub mod arch;
pub mod startup;
pub mod task;
pub mod time;
pub mod umem;

use self::task::*;
use self::umem::*;

/// Implementation of the SEND IPC primitive.
///
/// `caller` is a valid task index (i.e. not directly from user code).
///
/// # Panics
///
/// If `caller` is out of range for `tasks`.
pub fn send(tasks: &mut [Task], caller: usize) -> NextTask {
    // Extract callee.
    let callee = tasks[caller].save.as_send_args().callee();

    // Check IPC filter - TODO
    // Open question: should out-of-range task IDs be handled by faulting below,
    // or by failing the IPC filter? Either condition will fault...

    // Verify the given callee ID, converting it into a table index on success.
    let callee = match task::check_task_id_against_table(tasks, callee) {
        Err(task::TaskIDError::OutOfRange) => {
            return tasks[caller].force_fault(FaultInfo::SyscallUsage(
                UsageError::TaskOutOfRange,
            ));
        }
        Err(task::TaskIDError::Stale) => {
            // Inform caller by resuming it with an error response code.
            resume_sender_with_error(&mut tasks[caller]);
            return NextTask::Same;
        }
        Ok(i) => i,
    };

    // Check for ready peer.
    if let TaskState::Healthy(SchedState::InRecv(from)) = tasks[callee].state {
        if from.is_none() || from == Some(caller) {
            // Callee is waiting in receive -- either an open receive, or a
            // directed receive from just us. Either way, we can directly
            // deliver the message and switch tasks...unless either task was
            // naughty, in which case we have to fault it and block.
            match deliver(tasks, caller, callee) {
                Ok(_) => {
                    // Delivery succeeded!
                    // Block caller.
                    tasks[caller].state =
                        TaskState::Healthy(SchedState::InReply(callee));
                    // Unblock callee.
                    tasks[callee].state =
                        TaskState::Healthy(SchedState::Runnable);
                    // Propose switching directly to the unblocked callee.
                    return NextTask::Specific(callee);
                }
                Err(interact) => {
                    // Delivery failed because of fault events in one or both
                    // tasks. We need to apply the fault status, and then if we
                    // didn't have to murder the caller, we'll fall through to
                    // block it below.
                    if let Some(fault) = interact.recipient {
                        // Callee specified a bogus receive buffer. Bad callee.
                        let _hint = tasks[callee].force_fault(fault);
                    }
                    if let Some(fault) = interact.sender {
                        // Caller specified a bogus message buffer. Bad caller.
                        // We'll stop processing here, because we can't block
                        // the caller more than we just have (doing so would
                        // mangle the fault state).
                        return tasks[caller].force_fault(fault);
                    }
                    // If we didn't just return, fall through to the caller
                    // blocking code below.
                }
            }
        } else {
            // callee is blocked in receive, but it's a directed receive not
            // involving us, so we must treat it as busy and block the caller.
        }
    }

    // Caller needs to block sending, callee is either busy or
    // faulted.
    tasks[caller].state = TaskState::Healthy(SchedState::InSend(callee));
    // We don't know what the best task to run now would be, but
    // we're pretty darn sure it isn't the caller.
    return NextTask::Other;
}

pub fn recv(tasks: &mut [Task], caller: usize) -> NextTask {
    // We allow tasks to atomically replace their notification mask at each
    // receive. We simultaneously find out if there are notifications pending.
    let recv_args = tasks[caller].save.as_recv_args();
    let notmask = recv_args.notification_mask();
    drop(recv_args);

    if let Some(firing) = tasks[caller].update_mask(notmask) {
        // Pending! Deliver an artificial message from the kernel.
        let mut rr = tasks[caller].save.as_recv_result();
        rr.set_sender(TaskID::KERNEL);
        rr.set_operation(firing);
        rr.set_message_len(0);
        rr.set_response_capacity(0);
        rr.set_lease_count(0);
        tasks[caller].acknowledge_notifications();
        return NextTask::Same;
    }

    // Begin the search for tasks waiting to send to `caller`. This search needs
    // to be able to iterate because it's possible that some of these senders
    // have bogus arguments to receive, e.g. are trying to get us to deliver a
    // "message" from memory they don't own. The apparently infinite loop
    // terminates if:
    //
    // - A legit sender is found and its message can be delivered.
    // - A legit sender is found, but the *caller* misbehaved and gets faulted.
    // - No senders were found (after fault processing) and we have to block the
    //   caller.
    let sending_to_us = TaskState::Healthy(SchedState::InSend(caller));
    let mut last = caller; // keep track of scan position.
    loop {
        // Is anyone blocked waiting to send to us?
        let sender =
            task::priority_scan(last, tasks, |t| t.state == sending_to_us);
        if let Some(sender) = sender {
            // Oh hello sender!
            match deliver(tasks, sender, caller) {
                Ok(_) => {
                    // Delivery succeeded! Change the sender's blocking state.
                    tasks[sender].state =
                        TaskState::Healthy(SchedState::InReply(caller));
                    // And go ahead and let the caller resume.
                    return NextTask::Same;
                }
                Err(interact) => {
                    // Delivery failed because of fault events in one or both
                    // tasks.  We need to apply the fault status, and then if we
                    // didn't have to murder the caller, we'll retry receiving a
                    // message.
                    if let Some(fault) = interact.sender {
                        // Sender was blocked with bad arguments. Fault it
                        // without affecting scheduling.
                        let _hint = tasks[sender].force_fault(fault);
                    }
                    if let Some(fault) = interact.recipient {
                        // This is our caller, which means the args to receive
                        // were nonsense. Very disappointed in caller. Since
                        // caller was running, this scheduling hint gets
                        // returned straight away, aborting the search.
                        return tasks[caller].force_fault(fault);
                    }
                    // Okay, if we didn't just return, retry the search.
                    last = sender;
                }
            }
        } else {
            // No notifications, nobody waiting to send -- block the caller.
            tasks[caller].state = TaskState::Healthy(SchedState::InRecv(None));
            // We don't know what task should run next, but we're pretty sure it's
            // not the one we just blocked.
            return NextTask::Other;
        }
    }
}

pub fn reply(tasks: &mut [Task], caller: usize) -> NextTask {
    // Extract the target of the reply.
    let callee = tasks[caller].save.as_reply_args().callee();

    // Validate it. We tolerate stale IDs here (it's not the callee's fault if
    // the caller crashed before receiving its reply) but we treat invalid
    // indices that could never have been received as a malfunction.
    let callee = match task::check_task_id_against_table(tasks, callee) {
        Err(task::TaskIDError::OutOfRange) => {
            return tasks[caller].force_fault(FaultInfo::SyscallUsage(
                UsageError::TaskOutOfRange,
            ));
        }
        Err(task::TaskIDError::Stale) => {
            // Silently drop the reply.
            return NextTask::Same;
        }
        Ok(i) => i,
    };

    if tasks[callee].state != TaskState::Healthy(SchedState::InReply(caller)) {
        // Huh. The target task is off doing something else. This can happen if
        // application-specific supervisory logic unblocks it before we've had a
        // chance to reply (e.g. to implement timeouts).
        return NextTask::Same;
    }

    // Deliver the reply. Note that we can't use `deliver`, which is
    // specific to a pair of tasks that are sending and receiving,
    // respectively.

    // Collect information on the send from the caller. This information is
    // all stored in infallibly-readable areas, but our accesses can fail if
    // the caller handed us bogus slices.
    let reply_args = tasks[caller].save.as_reply_args();
    // Read the reply arg that could fault first.
    let src_slice = reply_args.message();
    let src_slice = if let Ok(ss) = src_slice {
        ss
    } else {
        // The task invoking reply handed us an illegal slice instead of a
        // valid reply message! Naughty naughty.
        return tasks[caller]
            .force_fault(FaultInfo::SyscallUsage(UsageError::InvalidSlice));
    };
    // Cool, now collect the rest and unborrow.
    let code = reply_args.response_code();
    drop(reply_args);

    // Collect information about the callee's reply buffer. This, too, is
    // somewhere we can read infallibly.
    let send_args = tasks[callee].save.as_send_args();
    let dest_slice = match send_args.response_buffer() {
        Ok(buffer) => buffer,
        Err(e) => {
            // The sender set up a bogus response buffer. How rude. This
            // doesn't affect scheduling, so discard the hint.
            let _ = tasks[caller].force_fault(FaultInfo::SyscallUsage(e));
            return NextTask::Same;
        }
    };
    drop(send_args);

    // Okay, ready to attempt the copy.
    // TODO: we want to treat any attempt to copy more than will fit as a fault
    // in the task that is replying, because it knows how big the target buffer
    // is and is expected to respect that. This is not currently implemented --
    // currently you'll get the prefix.
    let amount_copied =
        safe_copy(&tasks[caller], src_slice, &tasks[callee], dest_slice);
    let amount_copied = match amount_copied {
        Ok(n) => n,
        Err(interact) => {
            // Delivery failed because of fault events in one or both tasks.  We
            // need to apply the fault status, and possibly fault the caller.
            if let Some(fault) = interact.recipient {
                // The task we're replying to had a bogus response buffer. Oh
                // well! Fault it and move on.
                let _ = tasks[callee].force_fault(fault);
            }
            if let Some(fault) = interact.sender {
                // That's our caller! They gave illegal arguments to reply! And
                // to think we trusted them!
                return tasks[caller].force_fault(fault);
            } else {
                // Resume the caller without resuming the target task below.
                return NextTask::Same;
            }
        }
    };

    let mut send_result = tasks[callee].save.as_send_result();
    send_result.set_response_and_length(code, amount_copied);
    drop(send_result);

    tasks[callee].state = TaskState::Healthy(SchedState::Runnable);

    // KEY ASSUMPTION: sends go from less important tasks to more important
    // tasks. As a result, Reply doesn't have scheduling implications unless
    // the task using it faults.
    return NextTask::Same;
}

/// Transfers a message from caller's context into callee's. This may be called
/// in several contexts:
///
/// - During execution of a SEND syscall by caller, when callee was already
///   waiting in RECV.
/// - During execution of a RECV by callee, when caller was already waiting in a
///   SEND.
/// - If one task is waiting and the other is transitioned from faulted state
///   into a waiting state.
///
/// In other words, *do not* assume that either task is currently scheduled; the
/// third case occurs when *neither* task is scheduled.
///
/// Preconditions:
///
/// - Caller is sending -- either blocked in state `InSend`, or in the
///   process of transitioning from `Runnable` to `InReply`.
/// - Callee is receiving -- either blocked in `InRecv` or in `Runnable`
///   executing a receive system call.
///
/// Deliver may fail due to a fault in either or both task. In that case, it
/// will stuff the precise fault into the task's scheduling state and return
/// `Err` indicating that a task switch is required, under the assumption that
/// at least one of the tasks involved in the `deliver` call was running.
/// (Which, as noted above, is not strictly true in practice, but is pretty
/// close to true. The recovering-from-fault case can explicitly discard the
/// scheduling hint.)
///
/// On success, returns `Ok(())` and any task-switching is the caller's
/// responsibility.
fn deliver(
    tasks: &mut [Task],
    caller: usize,
    callee: usize,
) -> Result<(), InteractFault> {
    // Collect information on the send from the caller. This information is all
    // stored in infallibly-readable areas, but our accesses can fail if the
    // caller handed us bogus slices.
    let send_args = tasks[caller].save.as_send_args();
    let op = send_args.operation();
    let caller_id =
        TaskID::from_index_and_gen(caller, tasks[caller].generation);
    let src_slice = send_args.message().map_err(InteractFault::in_sender)?;
    let response_capacity = send_args
        .response_buffer()
        .map_err(InteractFault::in_sender)?
        .len();
    let lease_count = send_args
        .lease_table()
        .map_err(InteractFault::in_sender)?
        .len();
    drop(send_args);

    // Collect information about the callee's receive buffer. This, too, is
    // somewhere we can read infallibly.
    let recv_args = tasks[callee].save.as_recv_args();
    let dest_slice = recv_args.buffer().map_err(InteractFault::in_recipient)?;
    drop(recv_args);

    // Okay, ready to attempt the copy.
    let amount_copied =
        safe_copy(&tasks[caller], src_slice, &tasks[callee], dest_slice)?;
    let mut rr = tasks[callee].save.as_recv_result();
    rr.set_sender(caller_id);
    rr.set_operation(u32::from(op));
    rr.set_message_len(amount_copied);
    rr.set_response_capacity(response_capacity);
    rr.set_lease_count(lease_count);
    drop(rr);

    tasks[caller].state = TaskState::Healthy(SchedState::InReply(callee));
    tasks[callee].state = TaskState::Healthy(SchedState::Runnable);
    // We don't have an opinion about the newly runnable task, nor do we
    // have enough information to insist that a switch must happen.
    Ok(())
}

#[derive(Copy, Clone, Debug)]
pub struct InteractFault {
    pub sender: Option<FaultInfo>,
    pub recipient: Option<FaultInfo>,
}

impl InteractFault {
    fn in_sender(fi: impl Into<FaultInfo>) -> Self {
        Self {
            sender: Some(fi.into()),
            recipient: None,
        }
    }

    fn in_recipient(fi: impl Into<FaultInfo>) -> Self {
        Self {
            sender: None,
            recipient: Some(fi.into()),
        }
    }
}

/// Updates `task`'s registers to show that the send syscall failed.
///
/// This is factored out because I'm betting we're going to want it in a bunch
/// of places. That might prove wrong.
fn resume_sender_with_error(task: &mut Task) {
    let mut r = task.save.as_send_result();
    r.set_response_and_length(abi::DEAD, 0);
}

/// Implementation of the `TIMER` syscall.
pub fn timer(task: &mut Task, now: time::Timestamp) -> NextTask {
    let args = task.save.as_timer_args();
    let (dl, n) = (args.deadline(), args.notification());
    if let Some(deadline) = dl {
        // timer is being enabled
        if deadline <= now {
            // timer is already expired
            task.set_timer(None, n);
            // We don't care if we woke the task, because it's already running!
            let _ = task.post(n);
            return NextTask::Same
        }
    }
    task.set_timer(dl, n);
    NextTask::Same
}
