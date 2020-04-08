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

pub mod task;
pub mod time;
pub mod umem;

use self::task::*;
use self::umem::*;

/// Response code returned by the kernel to signal that an IPC failed because
/// the peer died.
pub const DEAD: u32 = !0;


/// Implementation of the SEND IPC primitive.
pub fn send(tasks: &mut [Task], caller: usize) -> NextTask {
    // Extract callee.
    let callee = tasks[caller].save.as_send_args().callee();
    // Check IPC filter - TODO
    // Check for dead task ID.
    if tasks[callee.index()].generation == callee.generation() {
        let callee = callee.index();
        // Check for ready peer.
        if let TaskState::Healthy(SchedState::Receiving(from)) = tasks[callee].state {
            if from.unwrap_or(caller) == caller {
                // We can skip a step, unless they fault...
                match deliver(tasks, caller, callee) {
                    Ok(_) => {
                        tasks[caller].state = TaskState::Healthy(SchedState::AwaitingReplyFrom(callee));
                        tasks[callee].state = TaskState::Healthy(SchedState::Runnable);
                        // Propose switching directly to the unblocked callee.
                        return NextTask::Specific(callee)
                    }
                    Err(DeliverError::CopyFault(faults)) => {
                        // Delivery failed. We need to apply the fault status,
                        // and then we'll fall through to the block handling
                        // code below.
                        if let Some(addr) = faults.dest_fault {
                            let _hint = tasks[callee].force_fault(FaultInfo::MemoryAccess {
                                address: Some(addr),
                                source: FaultSource::Kernel,
                            });
                        }
                        if let Some(addr) = faults.src_fault {
                            // We'll stop processing here to dodge the
                            // blocking-handing code below.
                            return tasks[caller].force_fault(FaultInfo::MemoryAccess {
                                address: Some(addr),
                                source: FaultSource::Kernel,
                            });
                        }
                    }
                    Err(DeliverError::BadSenderMessage) | Err(DeliverError::BadSenderReplyBuffer) | Err(DeliverError::BadLeaseTable) => {
                        // Sender gave bogus syscall arguments.
                        return tasks[caller].force_fault(FaultInfo::BadArgs);
                    }
                    Err(DeliverError::BadRecipientBuffer) => {
                        // Recipient is waiting in receive with a bogus buffer.
                        // Normally we would detect and block this as it enters
                        // RECV, but this condition can be manufactured by e.g.
                        // a malfunctioning debugger. So we handle it. We are
                        // deliberately ignoring the scheduler hint.
                        let _hint = tasks[callee].force_fault(FaultInfo::BadArgs);
                    }
                }
            }
        }

        // Caller needs to block sending, callee is either busy or
        // faulted.
        tasks[caller].state = TaskState::Healthy(SchedState::SendingTo(callee));
        // We don't know what the best task to run now would be, but
        // we're pretty darn sure it isn't the caller.
        return NextTask::Other
    } else {
        // Inform caller by resuming it with an error response code.
        resume_sender_with_error(&mut tasks[caller]);
        NextTask::Same
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
/// - Caller is sending -- either blocked in state `SendingTo`, or in the
///   process of transitioning from `Runnable` to `AwaitingReplyFrom`.
/// - Callee is receiving -- either blocked in `Receiving` or in `Runnable`
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
fn deliver(tasks: &mut [Task], caller: usize, callee: usize) -> Result<(), DeliverError> {
    // Collect information on the send from the caller. This information is all
    // stored in infallibly-readable areas, but our accesses can fail if the
    // caller handed us bogus slices.
    let send_args = tasks[caller].save.as_send_args();
    let op = send_args.operation();
    let caller_id = TaskID::from_index_and_gen(caller, tasks[caller].generation);
    let src_slice = send_args.message().ok_or(DeliverError::BadSenderMessage)?;
    let response_capacity = send_args.response_buffer()
        .ok_or(DeliverError::BadSenderReplyBuffer)?
        .len();
    let lease_count = send_args.lease_table()
        .ok_or(DeliverError::BadLeaseTable)?
        .len();
    drop(send_args);

    // Collect information about the callee's receive buffer. This, too, is
    // somewhere we can read infallibly.
    let recv_args = tasks[callee].save.as_recv_args();
    let dest_slice = recv_args.buffer()
        .ok_or(DeliverError::BadRecipientBuffer)?;
    drop(recv_args);

    // Okay, ready to attempt the copy.
    let amount_copied =
        safe_copy(&tasks[caller], src_slice, &tasks[callee], dest_slice)
        .map_err(DeliverError::CopyFault)?;
    let mut rr = tasks[callee].save.as_recv_result();
    rr.set_sender(caller_id);
    rr.set_operation(op);
    rr.set_message_len(amount_copied);
    rr.set_response_capacity(response_capacity);
    rr.set_lease_count(lease_count);
    drop(rr);

    tasks[caller].state = TaskState::Healthy(SchedState::AwaitingReplyFrom(callee));
    tasks[callee].state = TaskState::Healthy(SchedState::Runnable);
    // We don't have an opinion about the newly runnable task, nor do we
    // have enough information to insist that a switch must happen.
    Ok(())
}

enum DeliverError {
    CopyFault(CopyError),
    BadSenderMessage,
    BadSenderReplyBuffer,
    BadLeaseTable,
    BadRecipientBuffer,
}

/// Updates `task`'s registers to show that the send syscall failed.
///
/// This is factored out because I'm betting we're going to want it in a bunch
/// of places. That might prove wrong.
fn resume_sender_with_error(task: &mut Task) {
    let mut r = task.save.as_send_result();
    r.set_response_and_length(DEAD, 0);
}
