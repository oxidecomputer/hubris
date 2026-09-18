// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Syscall implementation for a task running as a host process.
//!
//! There is no kernel here. Every syscall is forwarded, as a blocking
//! request/response exchange over the process's stdin and stdout, to a
//! *fixture* process that plays the kernel and all other tasks. The protocol
//! lives in the `hostcall` crate; this module only translates between the
//! `sys_*` signatures and its messages.
//!
//! Because Hubris syscalls block and a task is single-threaded, the task's
//! view is unchanged: a syscall returns when the fixture answers. Where the
//! kernel would kill the task instead of returning, the fixture answers with a
//! fault and this process exits with [`hostcall::EXIT_FAULT`].
//!
//! Stdout belongs to the protocol. A task, or anything it links, must not
//! print to it; diagnostics go to stderr.

use std::io::{BufReader, Stdin, Stdout};
use std::sync::{Mutex, MutexGuard, OnceLock};

use abi::{
    IrqControlArg, IrqStatus, LeaseAttributes, ReplyFaultReason, TaskId,
};
use hostcall::nprpc::{ClientIoError, Response};
use hostcall::runtime::Client as _;
use hostcall::syscalls::Client as _;
use hostcall::{
    BorrowInfoRequest, BorrowReadRequest, BorrowWriteRequest, IoError,
    IrqControlRequest, Outcome, PanicRequest, PostRequest, RecvRequest,
    ReplyFaultRequest, ReplyRequest, SendRequest, SetTimerRequest,
};

use crate::{
    BorrowInfo, Lease, PANIC_MESSAGE_MAX_LEN, RecvMessage, TimerState,
};

type Fixture = hostcall::Client<BufReader<Stdin>, Stdout>;

static FIXTURE: OnceLock<Mutex<Fixture>> = OnceLock::new();

/// The connection to the fixture, opened on first use.
///
/// Opening it also installs the panic hook that reports panics to the
/// fixture.
fn fixture() -> MutexGuard<'static, Fixture> {
    let fixture = FIXTURE.get_or_init(|| {
        install_panic_hook();
        Mutex::new(hostcall::Client::stdio())
    });
    // A poisoned lock means a panic in the middle of an exchange, and the
    // panic hook has already tried to report it. Carry on regardless: the
    // client holds no protocol state that a panic could have corrupted.
    fixture.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reports panics to the fixture the way `sys_panic` reports explicit ones,
/// after letting the default hook print the usual message to stderr.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        // Like the on-target handler, keep at most PANIC_MESSAGE_MAX_LEN
        // bytes of the rendered message, even if that splits a character.
        let mut message = info.to_string().into_bytes();
        message.truncate(PANIC_MESSAGE_MAX_LEN);
        // If the panic happened while the fixture was locked (that is, inside
        // one of the functions below), we can't use the connection; the
        // exit status still tells the fixture what happened.
        if let Some(fixture) = FIXTURE.get()
            && let Ok(mut fixture) = fixture.try_lock()
        {
            let _ = fixture.panic(&PanicRequest { message });
        }
        std::process::exit(hostcall::EXIT_PANIC);
    }));
}

/// Performs one exchange with the fixture and unwraps its outcome.
///
/// A fault from the fixture or a broken transport ends the process, because
/// the task has no way to continue in either case; the kernel would have
/// killed it in the first and it would never be scheduled again in the second.
fn call<T, E: std::fmt::Debug>(
    what: &str,
    exchange: impl FnOnce(&mut Fixture) -> Result<Response<Outcome<T>>, E>,
) -> T {
    let mut fixture = fixture();
    let result = exchange(&mut fixture);
    drop(fixture);
    match result {
        Ok(response) => match response.body {
            Ok(value) => value,
            Err(fault) => {
                eprintln!(
                    "task killed by the fixture during {what}: {}",
                    fault.description
                );
                std::process::exit(hostcall::EXIT_FAULT);
            }
        },
        Err(e) => transport_failed(what, e),
    }
}

fn transport_failed(what: &str, e: impl std::fmt::Debug) -> ! {
    eprintln!("lost the fixture during {what}: {e:?}");
    std::process::exit(hostcall::EXIT_TRANSPORT);
}

/// Bytes of a lease as the task sees them.
///
/// # Safety
///
/// Only call with a lease built by one of the `Lease` constructors, which
/// borrow the slice for the lease's lifetime.
unsafe fn lease_bytes<'a>(lease: &'a Lease<'_>) -> &'a [u8] {
    let rep = &lease._kern_rep;
    unsafe {
        core::slice::from_raw_parts(rep.base_address.as_ptr(), rep.length)
    }
}

/// Overwrites the start of a writable lease with `data`, dropping whatever
/// doesn't fit.
///
/// # Safety
///
/// As for `lease_bytes`, and the lease must have been created from a `&mut`
/// slice, which `read_write` and `write_only` guarantee by their signatures.
unsafe fn write_lease_prefix(lease: &Lease<'_>, data: &[u8]) {
    let rep = &lease._kern_rep;
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            rep.base_address.as_mut_ptr(),
            rep.length,
        )
    };
    copy_prefix(bytes, data);
}

/// Copies as much of `src` as fits into `dst`, returning how much that was.
fn copy_prefix(dst: &mut [u8], src: &[u8]) -> usize {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
    n
}

pub fn sys_send(
    target: TaskId,
    operation: u16,
    outgoing: &[u8],
    incoming: &mut [u8],
    leases: &[Lease<'_>],
) -> (u32, usize) {
    let lease_table = leases
        .iter()
        .map(|lease| {
            let attributes = lease._kern_rep.attributes;
            hostcall::Lease {
                attributes: attributes.bits(),
                len: lease._kern_rep.length as u32,
                contents: if attributes.contains(LeaseAttributes::READ) {
                    // Safety: built by a Lease constructor.
                    unsafe { lease_bytes(lease) }.to_vec()
                } else {
                    Vec::new()
                },
            }
        })
        .collect();

    let response = call("send", |fixture| {
        fixture.send(&SendRequest {
            target: target.0,
            operation,
            message: outgoing.to_vec(),
            reply_capacity: incoming.len() as u32,
            leases: lease_table,
        })
    });

    // The kernel would fault a server whose reply exceeds our capacity; the
    // fixture is trusted to respect it, and anything extra is dropped.
    let reply_len = copy_prefix(incoming, &response.reply);

    for (lease, writeback) in leases.iter().zip(response.lease_writebacks) {
        let Some(data) = writeback else {
            continue;
        };
        if !lease._kern_rep.attributes.contains(LeaseAttributes::WRITE) {
            transport_failed(
                "send",
                "the fixture wrote back into a lease that is not writable",
            );
        }
        // Safety: writable leases come from `&mut [u8]`.
        unsafe { write_lease_prefix(lease, &data) };
    }

    (response.code, reply_len)
}

pub fn sys_recv(
    buffer: &mut [u8],
    notification_mask: u32,
    specific_sender: Option<TaskId>,
) -> Result<RecvMessage, u32> {
    let response = call("recv", |fixture| {
        fixture.recv(&RecvRequest {
            capacity: buffer.len() as u32,
            notification_mask,
            specific_sender: specific_sender.map(|t| t.0),
        })
    });
    match response {
        Ok(message) => {
            // The fixture truncated to our capacity; the honest length is
            // reported separately, as the kernel does.
            copy_prefix(buffer, &message.message);
            Ok(RecvMessage {
                sender: TaskId(message.sender),
                operation: message.operation,
                message_len: message.message_len as usize,
                response_capacity: message.response_capacity as usize,
                lease_count: message.lease_count as usize,
            })
        }
        // The kernel ABI says an open receive cannot fail, and callers rely
        // on that with `unreachable_unchecked`; don't let a fixture bug turn
        // into undefined behavior.
        Err(_) if specific_sender.is_none() => transport_failed(
            "recv",
            "the fixture failed an open receive, which cannot fail",
        ),
        Err(code) => Err(code),
    }
}

pub fn sys_reply(peer: TaskId, code: u32, message: &[u8]) {
    call("reply", |fixture| {
        fixture.reply(&ReplyRequest {
            peer: peer.0,
            code,
            message: message.to_vec(),
        })
    })
}

pub fn sys_set_timer(deadline: Option<u64>, notifications: u32) {
    call("set_timer", |fixture| {
        fixture.set_timer(&SetTimerRequest {
            deadline,
            notifications,
        })
    })
}

pub fn sys_borrow_read(
    lender: TaskId,
    index: usize,
    offset: usize,
    dest: &mut [u8],
) -> (u32, usize) {
    let response = call("borrow_read", |fixture| {
        fixture.borrow_read(&BorrowReadRequest {
            lender: lender.0,
            index: index as u32,
            offset: offset as u32,
            len: dest.len() as u32,
        })
    });
    let n = copy_prefix(dest, &response.data);
    (response.code, n)
}

pub fn sys_borrow_write(
    lender: TaskId,
    index: usize,
    offset: usize,
    src: &[u8],
) -> (u32, usize) {
    let response = call("borrow_write", |fixture| {
        fixture.borrow_write(&BorrowWriteRequest {
            lender: lender.0,
            index: index as u32,
            offset: offset as u32,
            data: src.to_vec(),
        })
    });
    (response.code, response.len as usize)
}

pub fn sys_borrow_info(lender: TaskId, index: usize) -> Option<BorrowInfo> {
    call("borrow_info", |fixture| {
        fixture.borrow_info(&BorrowInfoRequest {
            lender: lender.0,
            index: index as u32,
        })
    })
    .map(|info| BorrowInfo {
        attributes: LeaseAttributes::from_bits_truncate(info.attributes),
        len: info.len as usize,
    })
}

fn irq_control(mask: u32, flags: IrqControlArg) {
    call("irq_control", |fixture| {
        fixture.irq_control(&IrqControlRequest {
            mask,
            flags: flags.bits(),
        })
    })
}

pub fn sys_irq_control(mask: u32, enable: bool) {
    let mut arg = IrqControlArg::empty();
    if enable {
        arg |= IrqControlArg::ENABLED;
    }
    irq_control(mask, arg)
}

pub fn sys_irq_control_clear_pending(mask: u32, enable: bool) {
    let mut arg = IrqControlArg::CLEAR_PENDING;
    if enable {
        arg |= IrqControlArg::ENABLED;
    }
    irq_control(mask, arg)
}

pub fn sys_panic(msg: &[u8]) -> ! {
    let mut fixture = fixture();
    let result = fixture.panic(&PanicRequest {
        message: msg.to_vec(),
    });
    drop(fixture);
    if let Err(e) = result {
        let e: ClientIoError<IoError> = e;
        transport_failed("panic", e);
    }
    std::process::exit(hostcall::EXIT_PANIC)
}

pub fn sys_get_timer() -> TimerState {
    let state = call("get_timer", |fixture| fixture.get_timer(&()));
    TimerState {
        now: state.now,
        deadline: state.deadline,
        on_dl: state.on_deadline,
    }
}

pub fn sys_refresh_task_id(task_id: TaskId) -> TaskId {
    TaskId(call("refresh_task_id", |fixture| {
        fixture.refresh_task_id(&task_id.0)
    }))
}

pub fn sys_post(task_id: TaskId, bits: u32) -> u32 {
    call("post", |fixture| {
        fixture.post(&PostRequest {
            task: task_id.0,
            bits,
        })
    })
}

pub fn sys_reply_fault(task_id: TaskId, reason: ReplyFaultReason) {
    call("reply_fault", |fixture| {
        fixture.reply_fault(&ReplyFaultRequest {
            peer: task_id.0,
            reason: reason as u32,
        })
    })
}

pub fn sys_irq_status(mask: u32) -> IrqStatus {
    IrqStatus::from_bits_truncate(call("irq_status", |fixture| {
        fixture.irq_status(&mask)
    }))
}

/// Asks the fixture which task index a `task_slot!` name refers to.
pub(crate) fn resolve_task_slot(name: &str) -> u16 {
    call("task_slot", |fixture| fixture.task_slot(&name.to_string()))
}
