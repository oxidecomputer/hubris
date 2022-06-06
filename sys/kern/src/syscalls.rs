// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Architecture-independent syscall implementation.
//!
//! This builds on architecture-specific parts defined in the `arch::*` modules.
//!
//! # Syscall implementations
//!
//! With only a couple of exceptions, syscalls are implemented by functions with
//! the signature:
//!
//! ```ignore
//! fn syscall(tasks: &mut [Task], caller: usize) -> Result<NextTask, UserError>;
//! ```
//!
//! `tasks` is the task table, and `caller` is the index of the task within it
//! that triggered the syscall. On return, a `NextTask` indicates success with a
//! possible scheduling hint, while a `UserError` indicates a condition that
//! needs to either be returned as a response code or recorded as a fault. A
//! common wrapper takes care of the final side effects, reducing code in each
//! syscall.
//!
//! Arguments to syscalls need to be read from the `task.save()` structure where
//! the task's registers are stored. Each class of syscall has an *argument
//! struct* type to make this easy and safe, e.g. `task.save().as_send_args()`.
//! See the `task::ArchState` trait for details.

use core::convert::TryFrom;

use abi::{
    FaultInfo, LeaseAttributes, SchedState, Sysnum, TaskId, TaskState, ULease,
    UsageError,
};
use unwrap_lite::UnwrapLite;

use crate::arch;
use crate::err::{InteractFault, UserError};
use crate::startup::with_task_table;
use crate::task::{self, current_id, ArchState, NextTask, Task};
use crate::time::Timestamp;
use crate::umem::{safe_copy, USlice};

/// Entry point accessed by arch-specific syscall entry sequence.
///
/// Before calling this, task volatile state (e.g. callee-save registers on ARM)
/// must be stored safely into the `SavedState` struct of the `Task`.
///
/// `nr` is the syscall number passed from user code.
///
/// `task` is a pointer to the current Task.
///
/// # Safety
///
/// To use this, you must (1) ensure that the state described above is saved on
/// the way in and restored on the way out, (2) call this from the syscall
/// interrupt handler, only, and (3) not call it reentrantly.
#[no_mangle]
pub unsafe extern "C" fn syscall_entry(nr: u32, task: *mut Task) {
    crate::profiling::event_syscall_enter(nr);

    // The task pointer is about to alias our task table, at which point it
    // could not be dereferenced -- so we'll shed our ability to dereference it
    // immediately.
    let idx = {
        // Safety: we're trusting the interrupt entry routine to pass us a valid
        // task pointer.
        let t = unsafe { &*task };
        usize::from(t.descriptor().index)
    };

    with_task_table(|tasks| {
        match safe_syscall_entry(nr, idx, tasks) {
            // If we're returning to the same task, we're done!
            NextTask::Same => (),

            NextTask::Specific(i) => {
                // Safety: this is a valid task from the tasks table, meeting
                // switch_to's requirements.
                unsafe { switch_to(&mut tasks[i]) }
            }

            NextTask::Other => {
                let next = task::select(idx, tasks);
                // Safety: this is a valid task from the tasks table, meeting
                // switch_to's requirements.
                unsafe { switch_to(&mut tasks[next]) }
            }
        }
    });

    crate::profiling::event_syscall_exit();
}

/// Factored out of `syscall_entry` to encapsulate the bits that don't need
/// unsafe.
fn safe_syscall_entry(nr: u32, current: usize, tasks: &mut [Task]) -> NextTask {
    let res = match Sysnum::try_from(nr) {
        Ok(Sysnum::Send) => send(tasks, current),
        Ok(Sysnum::Recv) => recv(tasks, current).map_err(UserError::from),
        Ok(Sysnum::Reply) => reply(tasks, current).map_err(UserError::from),
        Ok(Sysnum::SetTimer) => Ok(set_timer(&mut tasks[current], arch::now())),
        Ok(Sysnum::BorrowRead) => borrow_read(tasks, current),
        Ok(Sysnum::BorrowWrite) => borrow_write(tasks, current),
        Ok(Sysnum::BorrowInfo) => borrow_info(tasks, current),
        Ok(Sysnum::IrqControl) => irq_control(tasks, current),
        Ok(Sysnum::Panic) => explicit_panic(tasks, current),
        Ok(Sysnum::GetTimer) => Ok(get_timer(&mut tasks[current], arch::now())),
        Ok(Sysnum::RefreshTaskId) => refresh_task_id(tasks, current),
        Ok(Sysnum::Post) => post(tasks, current),
        Ok(Sysnum::ReplyFault) => {
            reply_fault(tasks, current).map_err(UserError::from)
        }
        Err(_) => {
            // Bogus syscall number! That's a fault.
            Err(FaultInfo::SyscallUsage(UsageError::BadSyscallNumber).into())
        }
    };
    match res {
        Ok(nt) => nt,
        Err(UserError::Recoverable(code, hint)) => {
            tasks[current].save_mut().set_error_response(code);
            hint
        }
        Err(UserError::Unrecoverable(fault)) => {
            task::force_fault(tasks, current, fault)
        }
    }
}

/// Implementation of the SEND IPC primitive.
///
/// `caller` is a valid task index (i.e. not directly from user code).
///
/// # Panics
///
/// If `caller` is out of range for `tasks`.
fn send(tasks: &mut [Task], caller: usize) -> Result<NextTask, UserError> {
    // Extract callee.
    let callee_id = tasks[caller].save().as_send_args().callee;

    // Check IPC filter - TODO
    // Open question: should out-of-range task IDs be handled by faulting below,
    // or by failing the IPC filter? Either condition will fault...

    // Route kernel messages.
    if callee_id == TaskId::KERNEL {
        return crate::kipc::handle_kernel_message(tasks, caller);
    }

    // Verify the given callee ID, converting it into a table index on success.
    let callee = task::check_task_id_against_table(tasks, callee_id)?;

    // Check for ready peer.
    let mut next_task = NextTask::Same;
    let caller_id = current_id(tasks, caller);
    if tasks[callee].state().can_accept_message_from(caller_id) {
        // Callee is waiting in receive -- either an open receive, or a
        // closed receive from just us. Either way, we can directly deliver the
        // message and switch tasks...unless either task was naughty, in which
        // case we have to fault it and block.
        match deliver(tasks, caller, callee) {
            Ok(_) => {
                // Delivery succeeded! The initiating task is now blocked in
                // reply. Switch directly to the callee.
                return Ok(NextTask::Specific(callee));
            }
            Err(interact) => {
                // Delivery failed because of fault events in one or both
                // tasks. We need to apply the fault status, and then if we
                // didn't have to murder the caller, we'll fall through to
                // block it below.
                next_task = interact.apply_to_dst(tasks, callee)?;
                // If we didn't just return, fall through to the caller
                // blocking code below.
            }
        }
    }

    // Caller needs to block sending, callee is either busy or
    // faulted.
    tasks[caller].set_healthy_state(SchedState::InSend(callee_id));
    // We may not know what task to run next, but we're pretty sure it isn't the
    // caller.
    Ok(NextTask::Other.combine(next_task))
}

/// Implementation of the RECV IPC primitive.
///
/// `caller` is a valid task index (i.e. not directly from user code).
///
/// # Panics
///
/// If `caller` is out of range for `tasks`.
fn recv(tasks: &mut [Task], caller: usize) -> Result<NextTask, UserError> {
    // We allow tasks to atomically replace their notification mask at each
    // receive. We simultaneously find out if there are notifications pending.
    if let Some(firing) = tasks[caller].take_notifications() {
        // Pending! Deliver an artificial message from the kernel.
        tasks[caller].save_mut().set_recv_result(
            TaskId::KERNEL,
            firing,
            0,
            0,
            0,
        );
        return Ok(NextTask::Same);
    }

    let caller_id = current_id(tasks, caller);

    let specific_sender = tasks[caller].save().as_recv_args().specific_sender;

    let mut next_task = NextTask::Same; // update if we wake tasks

    if specific_sender == Some(TaskId::KERNEL) {
        // We've already checked for notifications, which is the only kind of
        // message the kernel emits. No need to check further; we'll fall
        // through to the block code below and wait for notification.
    } else if let Some(sender_id) = specific_sender {
        // Closed Receive

        // No need to do any sort of iterative scan. We've got three potential
        // outcomes here.

        // First possibility: that task you're asking about is DEAD.
        let sender_idx = task::check_task_id_against_table(tasks, sender_id)?;
        // Second possibility: task has a message for us.
        if tasks[sender_idx].state().is_sending_to(caller_id) {
            // Oh hello sender!
            match deliver(tasks, sender_idx, caller) {
                Ok(_) => {
                    // Delivery succeeded! Sender is now blocked in reply. Go ahead
                    // and let the caller resume.
                    return Ok(next_task);
                }
                Err(interact) => {
                    // Delivery failed because of fault events in one or both
                    // tasks.  We need to apply the fault status, and then if we
                    // didn't have to murder the caller, we'll retry receiving a
                    // message.
                    let wake_hint = interact.apply_to_src(tasks, sender_idx)?;
                    next_task = next_task.combine(wake_hint);
                }
            }
        }
    // Third possibility: we need to block; fall through below.
    } else {
        // Open Receive

        // Begin the search for tasks waiting to send to `caller`. This search
        // needs to be able to iterate because it's possible that some of these
        // senders have bogus arguments to receive, e.g. are trying to get us to
        // deliver a "message" from memory they don't own. The apparently
        // infinite loop terminates if:
        //
        // - A legit sender is found and its message can be delivered.
        // - A legit sender is found, but the *caller* misbehaved and gets
        //   faulted.
        // - No senders were found (after fault processing) and we have to block
        // the caller.
        let mut last = caller; // keep track of scan position.

        // Is anyone blocked waiting to send to us?
        while let Some(sender) = task::priority_scan(last, tasks, |t| {
            t.state().is_sending_to(caller_id)
        }) {
            // Oh hello sender!
            match deliver(tasks, sender, caller) {
                Ok(_) => {
                    // Delivery succeeded! Sender is now blocked in reply. Go ahead
                    // and let the caller resume.
                    return Ok(next_task);
                }
                Err(interact) => {
                    // Delivery failed because of fault events in one or both
                    // tasks.  We need to apply the fault status, and then if we
                    // didn't have to murder the caller, we'll retry receiving a
                    // message.
                    let wake_hint = interact.apply_to_src(tasks, sender)?;
                    next_task = next_task.combine(wake_hint);
                    // Okay, if we didn't just return, retry the search from a new
                    // position.
                    last = sender;
                }
            }
        }
    }

    // No notifications, nobody waiting to send -- block the caller.
    tasks[caller].set_healthy_state(SchedState::InRecv(specific_sender));
    // We may not know what task should run next, but we're pretty sure it's not
    // the one we just blocked.
    Ok(NextTask::Other.combine(next_task))
}

/// Implementation of the REPLY IPC primitive.
///
/// `caller` is a valid task index (i.e. not directly from user code).
///
/// # Panics
///
/// If `caller` is out of range for `tasks`.
fn reply(tasks: &mut [Task], caller: usize) -> Result<NextTask, FaultInfo> {
    // Extract the target of the reply.
    let reply_args = tasks[caller].save().as_reply_args();
    let callee = reply_args.callee;
    let caller_id = current_id(tasks, caller);

    // Validate it. We tolerate stale IDs here (it's not the callee's fault if
    // the caller crashed before receiving its reply) but we treat invalid
    // indices that could never have been received as a malfunction.
    let callee = match task::check_task_id_against_table(tasks, callee) {
        Err(UserError::Recoverable(_, hint)) => return Ok(hint),
        Err(UserError::Unrecoverable(f)) => return Err(f),
        Ok(x) => x,
    };

    if tasks[callee].state()
        != &TaskState::Healthy(SchedState::InReply(caller_id))
    {
        // Huh. The target task is off doing something else. This can happen if
        // application-specific supervisory logic unblocks it before we've had a
        // chance to reply (e.g. to implement timeouts).
        return Ok(NextTask::Same);
    }

    // Deliver the reply. Note that we can't use `deliver`, which is
    // specific to a pair of tasks that are sending and receiving,
    // respectively.

    // Collect information on the send from the caller. This information is
    // all stored in infallibly-readable areas, but our accesses can fail if
    // the caller handed us bogus slices.
    //
    // Read the reply arg that could fault first.
    let src_slice = reply_args.message.map_err(|_| {
        // The task invoking reply handed us an illegal slice instead of a
        // valid reply message! Naughty naughty.
        FaultInfo::SyscallUsage(UsageError::InvalidSlice)
    })?;

    // Collect information about the callee's reply buffer. This, too, is
    // somewhere we can read infallibly.
    let send_args = tasks[callee].save().as_send_args();
    let dest_slice = match send_args.response {
        Ok(buffer) => buffer,
        Err(e) => {
            // The sender set up a bogus response buffer. How rude. This
            // may well affect scheduling if it wakes the supervisor, but is Ok
            // from our caller's perspective:
            return Ok(task::force_fault(
                tasks,
                callee,
                FaultInfo::SyscallUsage(e),
            ));
        }
    };

    // Okay, ready to attempt the copy.
    // TODO: we want to treat any attempt to copy more than will fit as a fault
    // in the task that is replying, because it knows how big the target buffer
    // is and is expected to respect that. This is not currently implemented --
    // currently you'll get the prefix.
    let amount_copied = safe_copy(tasks, caller, src_slice, callee, dest_slice);
    let amount_copied = match amount_copied {
        Ok(n) => n,
        Err(interact) => {
            // Delivery failed because of fault events in one or both tasks.  We
            // need to apply the fault status, and possibly fault the caller.
            let wake_hint = interact.apply_to_dst(tasks, callee)?;
            // If we didn't just return, resume the caller without resuming the
            // target task below.
            return Ok(wake_hint);
        }
    };

    tasks[callee]
        .save_mut()
        .set_send_response_and_length(reply_args.response_code, amount_copied);
    tasks[callee].set_healthy_state(SchedState::Runnable);

    // KEY ASSUMPTION: sends go from less important tasks to more important
    // tasks. As a result, Reply doesn't have scheduling implications unless
    // the task using it faults.
    Ok(NextTask::Same)
}

/// Implementation of the `SET_TIMER` syscall.
fn set_timer(task: &mut Task, now: Timestamp) -> NextTask {
    let args = task.save().as_set_timer_args();
    if let Some(deadline) = args.deadline {
        // timer is being enabled
        if deadline <= now {
            // timer is already expired
            task.set_timer(None, args.notification);
            // We don't care if we woke the task, because it's already running!
            let _ = task.post(args.notification);
            return NextTask::Same;
        }
    }
    task.set_timer(args.deadline, args.notification);
    NextTask::Same
}

/// Implementation of the `GET_TIMER` syscall.
fn get_timer(task: &mut Task, now: Timestamp) -> NextTask {
    // This syscall takes no arguments.

    let (dl, n) = task.timer();

    task.save_mut().set_time_result(now, dl, n);
    NextTask::Same
}

fn borrow_read(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, UserError> {
    // Collect parameters from caller.
    let args = tasks[caller].save().as_borrow_args();
    let buffer = args.buffer?;

    let lender = task::check_task_id_against_table(tasks, args.lender)?;

    let lease =
        borrow_lease(tasks, caller, lender, args.lease_number, args.offset)?;

    // Does the lease grant us the ability to read from the memory?
    if !lease.attributes.contains(LeaseAttributes::READ) {
        // Lease is not readable. Defecting lender.
        return Err(UserError::Recoverable(abi::DEFECT, NextTask::Same));
    }

    let leased_area = USlice::from(&lease);

    // Note: we do not explicitly check that the lender has access to
    // `leased_area` because `safe_copy` will do it.

    // Okay, goodness! We're finally getting close!
    let copy_result = safe_copy(tasks, lender, leased_area, caller, buffer);

    match copy_result {
        Ok(n) => {
            // Copy succeeded!
            tasks[caller]
                .save_mut()
                .set_borrow_response_and_length(0, n);
            Ok(NextTask::Same)
        }
        Err(interact) => {
            let wake_hint = interact.apply_to_src(tasks, lender)?;
            // Copy failed but not our side, report defecting lender.
            Err(UserError::Recoverable(abi::DEFECT, wake_hint))
        }
    }
}

fn borrow_write(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, UserError> {
    // Collect parameters from caller.
    let args = tasks[caller].save().as_borrow_args();
    let buffer = args.buffer?;

    let lender = task::check_task_id_against_table(tasks, args.lender)?;

    let lease =
        borrow_lease(tasks, caller, lender, args.lease_number, args.offset)?;

    // Does the lease grant us the ability to write to the memory?
    if !lease.attributes.contains(LeaseAttributes::WRITE) {
        // Lease is not readable. Defecting lender.
        return Err(UserError::Recoverable(abi::DEFECT, NextTask::Same));
    }

    let leased_area = USlice::from(&lease);

    // Note: we do not explicitly check that the lender has access to
    // `leased_area` because `safe_copy` will do it.

    // Okay, goodness! We're finally getting close!
    let copy_result = safe_copy(tasks, caller, buffer, lender, leased_area);

    match copy_result {
        Ok(n) => {
            // Copy succeeded!
            tasks[caller]
                .save_mut()
                .set_borrow_response_and_length(0, n);
            Ok(NextTask::Same)
        }
        Err(interact) => {
            let wake_hint = interact.apply_to_dst(tasks, lender)?;
            // Copy failed but not our side, report defecting lender.
            Err(UserError::Recoverable(abi::DEFECT, wake_hint))
        }
    }
}

fn borrow_info(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, UserError> {
    // Collect parameters from caller.
    let args = tasks[caller].save().as_borrow_args();

    let lender = task::check_task_id_against_table(tasks, args.lender)?;

    let lease = borrow_lease(tasks, caller, lender, args.lease_number, 0)?;

    tasks[caller]
        .save_mut()
        .set_borrow_info(lease.attributes.bits(), lease.length as usize);
    Ok(NextTask::Same)
}

fn borrow_lease(
    tasks: &mut [Task],
    caller: usize,
    lender: usize,
    lease_number: usize,
    offset: usize,
) -> Result<ULease, UserError> {
    let caller_id = current_id(tasks, caller);

    // Check state of lender and range of lease table.
    if tasks[lender].state()
        != &TaskState::Healthy(SchedState::InReply(caller_id))
    {
        // The alleged lender isn't lending anything at all.
        // Let's assume this is a defecting lender.
        return Err(UserError::Recoverable(abi::DEFECT, NextTask::Same));
    }

    let largs = tasks[lender].save().as_send_args();
    let leases = match largs.lease_table {
        Ok(t) => t,
        Err(e) => {
            // Huh. Lender has a corrupt lease table. This would normally be
            // caught during entry to SEND, but could occur if the task's state
            // has been rewritten by something (say, a debugger).
            let wake_hint =
                task::force_fault(tasks, lender, FaultInfo::SyscallUsage(e));
            return Err(UserError::Recoverable(abi::DEFECT, wake_hint));
        }
    };

    // Can the lender actually read the lease table, or are they being sneaky?
    let leases = match tasks[lender].try_read(&leases) {
        Ok(slice) => Ok(slice),
        Err(fault) => {
            let wake_hint = task::force_fault(tasks, lender, fault);
            Err(UserError::Recoverable(abi::DEFECT, wake_hint))
        }
    }?;

    // Try reading the lease. This is unsafe in the general case, but since
    // we've just convinced ourselves that the lease table is in task memory,
    // we can do this safely.
    let lease = leases.get(lease_number).cloned();
    // Is the lease number provided by the borrower legitimate?
    if let Some(mut lease) = lease {
        // Attempt to offset the lease. Handle cases where the offset is bogus.
        // First, we must convert to u32, which _should be_ a no-op but we'll do
        // it the careful way:
        let offset = u32::try_from(offset).unwrap_lite();
        // Now, proceed only if both neither the length nor address computation
        // wrap.
        if let (Some(off_len), Some(off_addr)) = (
            lease.length.checked_sub(offset),
            lease.base_address.checked_add(offset),
        ) {
            lease.base_address = off_addr;
            lease.length = off_len;
            Ok(lease)
        } else {
            Err(FaultInfo::SyscallUsage(UsageError::OffsetOutOfRange).into())
        }
    } else {
        // Borrower provided an invalid lease number. Borrower was told the
        // number of leases on successful RECV and should respect that. (Note:
        // if the lender's lease table changed shape, this will fault the
        // borrower, which might be bad.)
        Err(FaultInfo::SyscallUsage(UsageError::LeaseOutOfRange).into())
    }
}

/// Performs the architecture-specific bookkeeping to activate `task` on next
/// return to user. This should be done "on our way out" to user code, toward
/// the end of the syscall routine.
///
/// Note that this does *not* magically run user code. This is not Unix `swtch`.
///
/// # Safety
///
/// This is unsafe for two reasons.
///
/// 1. It will reconfigure the MPU according to instructions in `task`, which
///    could break kernel invariants if done maliciously.
/// 2. It will leak a pointer to `task` into static space, where it will be used
///    on next kernel entry.
///
/// To avoid causing problems, ensure that `task` is a member of the task table,
/// with memory protection generated by the build system, and that your access
/// to `task` goes out of scope before next kernel entry.
unsafe fn switch_to(task: &mut Task) {
    arch::apply_memory_protection(task);
    // Safety: our contract above is sufficient to ensure that this is safe.
    unsafe {
        arch::set_current_task(task);
    }
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
/// On success, updates the state of each task to finish delivery, and returns
/// `Ok(())`. Task-switching is the caller's responsibility, because we don't
/// have enough information here.
fn deliver(
    tasks: &mut [Task],
    caller: usize,
    callee: usize,
) -> Result<(), InteractFault> {
    let caller_id = task::current_id(tasks, caller);

    // Collect information on the send from the caller. This information is all
    // stored in infallibly-readable areas, but our accesses can fail if the
    // caller handed us bogus slices.
    let send_args = tasks[caller].save().as_send_args();
    let src_slice = send_args.message.map_err(InteractFault::in_src)?;
    let response_capacity =
        send_args.response.map_err(InteractFault::in_src)?.len();
    let lease_count =
        send_args.lease_table.map_err(InteractFault::in_src)?.len();

    // Collect information about the callee's receive buffer. This, too, is
    // somewhere we can read infallibly.
    let recv_args = tasks[callee].save().as_recv_args();
    let dest_slice = recv_args.buffer.map_err(InteractFault::in_dst)?;

    // Okay, ready to attempt the copy.
    let amount_copied =
        safe_copy(tasks, caller, src_slice, callee, dest_slice)?;
    tasks[callee].save_mut().set_recv_result(
        caller_id,
        u32::from(send_args.operation),
        amount_copied,
        response_capacity,
        lease_count,
    );

    let callee_id = current_id(tasks, callee);
    tasks[caller].set_healthy_state(SchedState::InReply(callee_id));
    tasks[callee].set_healthy_state(SchedState::Runnable);
    // We don't have an opinion about the newly runnable task, nor do we
    // have enough information to insist that a switch must happen.
    Ok(())
}

fn irq_control(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, UserError> {
    let args = tasks[caller].save().as_irq_args();

    let operation = match args.control {
        0 => crate::arch::disable_irq,
        1 => crate::arch::enable_irq,
        _ => {
            return Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
                UsageError::NoIrq,
            )))
        }
    };

    let caller = caller as u32;

    let irqs = crate::startup::HUBRIS_TASK_IRQ_LOOKUP
        .get(abi::InterruptOwner {
            task: caller,
            notification: args.notification_bitmask,
        })
        .ok_or(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::NoIrq,
        )))?;
    for i in irqs.iter() {
        operation(i.0);
    }
    Ok(NextTask::Same)
}

fn explicit_panic(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, UserError> {
    // It's the easiest syscall!
    Ok(task::force_fault(tasks, caller, FaultInfo::Panic))
}

fn refresh_task_id(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, UserError> {
    let peer = tasks[caller].save().as_refresh_task_id_args().task_id;
    let peer = peer.index(); // discard original generation

    if peer < tasks.len() {
        let tid = task::current_id(tasks, peer);
        tasks[caller].save_mut().set_refresh_task_id_result(tid);
        Ok(NextTask::Same)
    } else {
        Err(UserError::Unrecoverable(FaultInfo::SyscallUsage(
            UsageError::TaskOutOfRange,
        )))
    }
}

fn post(tasks: &mut [Task], caller: usize) -> Result<NextTask, UserError> {
    let args = tasks[caller].save().as_post_args();
    let peer_id = args.task_id;

    let peer_idx = task::check_task_id_against_table(tasks, peer_id)?;

    let woke = tasks[peer_idx].post(args.notification_bits);

    tasks[caller].save_mut().set_error_response(0);

    // In order to maintain the scheduler invariant that the highest priority
    // task is always running, we need to force a reschedule here. We could do
    // that unconditionally by just returning `Ok(woke)`, but that will waste
    // CPU if the notification is going from higher to lower priority -- and we
    // expect that to be the common case.
    //
    // And so, we will be slightly clever here.
    let caller_p = tasks[caller].priority();
    let peer_p = tasks[peer_idx].priority();
    if woke && peer_p.is_more_important_than(caller_p) {
        Ok(NextTask::Specific(peer_idx))
    } else {
        Ok(NextTask::Same)
    }
}

/// Implementation of the `REPLY_FAULT` IPC primitive.
///
/// `caller` is a valid task index (i.e. not directly from user code).
///
/// # Panics
///
/// If `caller` is out of range for `tasks`.
fn reply_fault(
    tasks: &mut [Task],
    caller: usize,
) -> Result<NextTask, FaultInfo> {
    let caller_id = current_id(tasks, caller);

    // Extract the target of the reply and the cited reason. This also validates
    // the syscall parameters before doing other validation.
    let args = tasks[caller].save().as_reply_fault_args();
    let reason = args.reason?;

    // Validate task ID. We tolerate stale IDs here (it's not the callee's fault
    // if the caller crashed before receiving its reply) but we treat invalid
    // indices that could never have been received as a malfunction.
    let callee = match task::check_task_id_against_table(tasks, args.callee) {
        Err(UserError::Recoverable(_, hint)) => return Ok(hint),
        Err(UserError::Unrecoverable(f)) => return Err(f),
        Ok(x) => x,
    };

    if tasks[callee].state()
        != &TaskState::Healthy(SchedState::InReply(caller_id))
    {
        // Huh. The target task is off doing something else. This can happen if
        // application-specific supervisory logic unblocks it before we've had a
        // chance to reply (e.g. to implement timeouts).
        return Ok(NextTask::Same);
    }

    // Check and deliver the fault. We explicitly discard its scheduling hint,
    // because the caller is lower priority than we are.
    let _hint = task::force_fault(
        tasks,
        callee,
        FaultInfo::FromServer(caller_id, reason),
    );

    // KEY ASSUMPTION: sends go from less important tasks to more important
    // tasks. As a result, Reply doesn't have scheduling implications unless
    // the task using it faults.
    Ok(NextTask::Same)
}
