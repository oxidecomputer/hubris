// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Syscall implementation for a task running as a host process.
//!
//! This uses the [`hostcall`] crate for communicating to a host-native `kern`
//! impl over stdio.
//!
//! We expect the kernel to spawn us, and listen to our stdout for syscall
//! requests, and to give us syscall return values via our stdin.

use std::io::{BufReader, Stdin, Stdout};
use std::sync::{Mutex, MutexGuard, OnceLock};

use abi::{
    IrqControlArg, IrqStatus, LeaseAttributes, ReplyFaultReason, Sysnum, TaskId,
};
use hostcall::{IoError, Return, Syscall};

use super::Arch;
use crate::{
    BorrowInfo, Lease, PANIC_MESSAGE_MAX_LEN, RecvMessage, TimerState,
};

/// The pipes to and from the kernel.
struct KernIo {
    reader: BufReader<Stdin>,
    writer: Stdout,
}

impl KernIo {
    /// Makes a syscall and waits for the fixture to let the task continue.
    fn exchange(&mut self, syscall: &Syscall) -> Result<Return, IoError> {
        hostcall::write_message(&mut self.writer, syscall)?;
        hostcall::read_message(&mut self.reader)?.ok_or(IoError::Eof)
    }
}

/// Singleton for storing the stdout/stdin handles
static KERN_IO: OnceLock<Mutex<KernIo>> = OnceLock::new();

/// Obtains the comms interface with the kernel.
///
/// On first call, this also installs the kernel panic hook that informs the
/// kernel that our task has panicked.
fn kernel_comms() -> MutexGuard<'static, KernIo> {
    let io = KERN_IO.get_or_init(|| {
        install_panic_hook();
        Mutex::new(KernIo {
            reader: BufReader::new(std::io::stdin()),
            writer: std::io::stdout(),
        })
    });
    // A poisoned lock means a panic in the middle of an exchange, so just hard
    // exit with the EXIT_PANIC return code.
    io.lock()
        .unwrap_or_else(|_e| std::process::exit(hostcall::EXIT_PANIC))
}

/// Installs a panic hook that reports task panics to the kernel, after running
/// the typical panic handler which prints the panic details to stderr.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        // Like the on-target handler, keep at most PANIC_MESSAGE_MAX_LEN
        // bytes of the rendered message, even if that splits a character.
        let message = info.to_string();
        let message = message.as_bytes();
        let len = message.len().min(PANIC_MESSAGE_MAX_LEN);

        // Do the panic syscall to inform the kernel we have panicked, then
        // close up shop.
        //
        // TODO: Do we just want to write the syscall (using `write_message`
        // directly) and bail without trying to wait for a response? Using
        // `syscall` for convenience here instead for now.
        let _ = syscall(Syscall::panic(&message[..len]));

        // Whether it worked or not, it's time to bail.
        std::process::exit(hostcall::EXIT_PANIC);
    }));
}

/// Perform a syscall and obtain the result from the kernel.
///
/// If there was an error communicating with the kernel (including end of file
/// or a malformed message), the task will panic and exit.
fn syscall(call: Syscall) -> Return {
    let mut fixture = kernel_comms();
    let result = fixture.exchange(&call);
    drop(fixture);
    result.unwrap_or_else(|e| fatal_syscall_error(call.nr, e))
}

/// We had something go terribly wrong while serving a system call, likely
/// during comms with the kernel.
///
/// Print an error to stderr, and kill the task.
fn fatal_syscall_error(what: abi::Sysnum, e: impl std::fmt::Debug) -> ! {
    let s = match what {
        Sysnum::Send => "send",
        Sysnum::Recv => "recv",
        Sysnum::Reply => "reply",
        Sysnum::SetTimer => "set_timer",
        Sysnum::BorrowRead => "borrow_read",
        Sysnum::BorrowWrite => "borrow_write",
        Sysnum::BorrowInfo => "borrow_info",
        Sysnum::IrqControl => "irq_control",
        Sysnum::Panic => "panic",
        Sysnum::GetTimer => "get_timer",
        Sysnum::RefreshTaskId => "refresh_task_id",
        Sysnum::Post => "post",
        Sysnum::ReplyFault => "reply_fault",
        Sysnum::IrqStatus => "irq_status",
    };
    eprintln!("fatal error during syscall {s}: {e:?}");
    std::process::exit(hostcall::EXIT_TRANSPORT);
}

/// Copies as much of `src` as fits into `dst`, returning how many bytes were
/// copied.
fn copy_prefix(dst: &mut [u8], src: &[u8]) -> usize {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
    n
}

/// Copies the first `len` bytes the fixture returned into `dst`, or as many
/// of them as there are and fit, and returns how many bytes were copied.
fn copy_output(dst: &mut [u8], ret: &Return, len: u32) -> usize {
    let len = (len as usize).min(ret.output.len());
    copy_prefix(dst, &ret.output[..len])
}

/// Bytes of a lease as the task sees them.
///
/// # Safety
///
/// Only call with a lease built by one of the `Lease` constructors, which
/// borrow the slice for the lease's lifetime.
unsafe fn lease_as_bytes<'a>(lease: &'a Lease<'_>) -> &'a [u8] {
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
/// Same requirements as `lease_as_bytes`, and the lease must have been created
/// from a `&mut` slice, which `read_write` and `write_only` guarantee by their
/// signatures.
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

/// The host-native implementation of a task's `userlib::Arch` interface.
///
/// Based on the [`hostcall`] crate's protocol primitives.
pub struct Host;

impl Arch for Host {
    fn send(
        target: TaskId,
        operation: u16,
        outgoing: &[u8],
        incoming: &mut [u8],
        leases: &[Lease<'_>],
    ) -> (u32, usize) {
        // Convert `userlib::Lease`s into `hostcall::Lease`s.
        let lease_table = leases
            .iter()
            .map(|lease| {
                let attributes = lease._kern_rep.attributes;
                hostcall::Lease {
                    attributes,
                    len: lease._kern_rep.length as u32,
                    contents: if attributes.contains(LeaseAttributes::READ) {
                        // Safety: built by a Lease constructor.
                        unsafe { lease_as_bytes(lease) }.to_vec()
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect::<Vec<_>>();

        let ret = syscall(Syscall::send(
            target,
            operation,
            outgoing,
            incoming,
            &lease_table,
        ));
        let [code, reply_len, ..] = ret.rets;
        let reply_len = copy_output(incoming, &ret, reply_len);

        for (lease, writeback) in leases.iter().zip(ret.lease_writebacks) {
            let Some(data) = writeback else {
                continue;
            };
            if !lease._kern_rep.attributes.contains(LeaseAttributes::WRITE) {
                fatal_syscall_error(
                    abi::Sysnum::Send,
                    "the fixture wrote back into a lease that is not writable",
                );
            }
            // Safety: writable leases come from `&mut [u8]`.
            unsafe { write_lease_prefix(lease, &data) };
        }

        (code, reply_len)
    }

    fn recv(
        buffer: &mut [u8],
        notification_mask: u32,
        specific_sender: Option<TaskId>,
    ) -> Result<RecvMessage, u32> {
        let ret = syscall(Syscall::recv(
            buffer.len(),
            notification_mask,
            specific_sender,
        ));
        let [code, sender, operation, len, response_capacity, lease_count] =
            ret.rets;
        match code {
            0 => {
                // Copy at most `len` items, but ignore the actual number of
                // bytes written, returning the `message_len` given to us by
                // the kernel instead.
                copy_output(buffer, &ret, len);

                Ok(RecvMessage {
                    sender: TaskId(sender as u16),
                    operation,
                    message_len: len as usize,
                    response_capacity: response_capacity as usize,
                    lease_count: lease_count as usize,
                })
            }
            // The kernel ABI says an open receive cannot fail, and callers rely
            // on that with `unreachable_unchecked`, abort if this condition is
            // violated.
            _ if specific_sender.is_none() => fatal_syscall_error(
                abi::Sysnum::Recv,
                "the fixture failed an open receive, which cannot fail",
            ),
            code => Err(code),
        }
    }

    fn reply(peer: TaskId, code: u32, message: &[u8]) {
        syscall(Syscall::reply(peer, code, message));
    }

    fn set_timer(deadline: Option<u64>, notifications: u32) {
        syscall(Syscall::set_timer(deadline, notifications));
    }

    fn borrow_read(
        lender: TaskId,
        index: usize,
        offset: usize,
        dest: &mut [u8],
    ) -> (u32, usize) {
        let ret =
            syscall(Syscall::borrow_read(lender, index, offset, dest.len()));
        let [code, num_copied, ..] = ret.rets;
        (code, copy_output(dest, &ret, num_copied))
    }

    fn borrow_write(
        lender: TaskId,
        index: usize,
        offset: usize,
        src: &[u8],
    ) -> (u32, usize) {
        let [code, num_copied, ..] =
            syscall(Syscall::borrow_write(lender, index, offset, src)).rets;
        (code, num_copied as usize)
    }

    fn borrow_info(lender: TaskId, index: usize) -> Option<BorrowInfo> {
        let [code, attributes, len, ..] =
            syscall(Syscall::borrow_info(lender, index)).rets;
        (code == 0).then(|| BorrowInfo {
            attributes: LeaseAttributes::from_bits_truncate(attributes),
            len: len as usize,
        })
    }

    fn irq_control(mask: u32, enable: bool) {
        let mut arg = IrqControlArg::empty();
        if enable {
            arg |= IrqControlArg::ENABLED;
        }
        syscall(Syscall::irq_control(mask, arg));
    }

    fn irq_control_clear_pending(mask: u32, enable: bool) {
        let mut arg = IrqControlArg::CLEAR_PENDING;
        if enable {
            arg |= IrqControlArg::ENABLED;
        }
        syscall(Syscall::irq_control(mask, arg));
    }

    fn panic(msg: &[u8]) -> ! {
        // TODO: Do we just want to write the syscall (using `write_message`
        // directly) and bail without trying to wait for a response? Using
        // `syscall` for convenience here instead for now.

        // Like the on-target handler, keep at most PANIC_MESSAGE_MAX_LEN
        // bytes of the rendered message, even if that splits a character.
        let len = msg.len().min(PANIC_MESSAGE_MAX_LEN);
        syscall(Syscall::panic(&msg[..len]));

        std::process::exit(hostcall::EXIT_PANIC)
    }

    fn get_timer() -> TimerState {
        let [now_lo, now_hi, set, dl_lo, dl_hi, on_dl] =
            syscall(Syscall::get_timer()).rets;
        let wide = |lo: u32, hi: u32| u64::from(hi) << 32 | u64::from(lo);
        TimerState {
            now: wide(now_lo, now_hi),
            deadline: (set != 0).then(|| wide(dl_lo, dl_hi)),
            on_dl,
        }
    }

    fn refresh_task_id(task_id: TaskId) -> TaskId {
        let [id, ..] = syscall(Syscall::refresh_task_id(task_id)).rets;
        TaskId(id as u16)
    }

    fn post(task_id: TaskId, bits: u32) -> u32 {
        let [code, ..] = syscall(Syscall::post(task_id, bits)).rets;
        code
    }

    fn reply_fault(task_id: TaskId, reason: ReplyFaultReason) {
        syscall(Syscall::reply_fault(task_id, reason));
    }

    fn irq_status(mask: u32) -> IrqStatus {
        let [stat, ..] = syscall(Syscall::irq_status(mask)).rets;
        IrqStatus::from_bits_truncate(stat)
    }
}
