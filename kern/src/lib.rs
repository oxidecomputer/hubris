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

pub mod arch;
pub mod task;
pub mod time;
pub mod umem;

use self::task::*;
use self::umem::*;

/// Response code returned by the kernel to signal that an IPC failed because
/// the peer died.
pub const DEAD: u32 = !0;


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
                    UsageError::TaskOutOfRange
            ));
        }
        Err(task::TaskIDError::Stale) => {
            // Inform caller by resuming it with an error response code.
            resume_sender_with_error(&mut tasks[caller]);
            return NextTask::Same
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
                    tasks[caller].state = TaskState::Healthy(
                        SchedState::InReply(callee)
                    );
                    // Unblock callee.
                    tasks[callee].state = TaskState::Healthy(
                        SchedState::Runnable
                    );
                    // Propose switching directly to the unblocked callee.
                    return NextTask::Specific(callee)
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
    return NextTask::Other
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
fn deliver(tasks: &mut [Task], caller: usize, callee: usize) -> Result<(), InteractFault> {
    // Collect information on the send from the caller. This information is all
    // stored in infallibly-readable areas, but our accesses can fail if the
    // caller handed us bogus slices.
    let send_args = tasks[caller].save.as_send_args();
    let op = send_args.operation();
    let caller_id = TaskID::from_index_and_gen(caller, tasks[caller].generation);
    let src_slice = send_args.message().map_err(InteractFault::in_sender)?;
    let response_capacity = send_args.response_buffer()
        .map_err(InteractFault::in_sender)?
        .len();
    let lease_count = send_args.lease_table()
        .map_err(InteractFault::in_sender)?
        .len();
    drop(send_args);

    // Collect information about the callee's receive buffer. This, too, is
    // somewhere we can read infallibly.
    let recv_args = tasks[callee].save.as_recv_args();
    let dest_slice = recv_args.buffer()
        .map_err(InteractFault::in_recipient)?;
    drop(recv_args);

    // Okay, ready to attempt the copy.
    let amount_copied =
        safe_copy(&tasks[caller], src_slice, &tasks[callee], dest_slice)?;
    let mut rr = tasks[callee].save.as_recv_result();
    rr.set_sender(caller_id);
    rr.set_operation(op);
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
    r.set_response_and_length(DEAD, 0);
}
