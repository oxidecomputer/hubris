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
use std::sync::{LazyLock, Mutex};

use abi::{
    IrqControlArg, IrqStatus, LeaseAttributes, ReplyFaultReason, TaskId,
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
    /// Makes a syscall and waits for the kernel to let the task continue.
    fn exchange(&mut self, syscall: &Syscall) -> Result<Return, IoError> {
        hostcall::write_message(&mut self.writer, syscall)?;
        hostcall::read_message(&mut self.reader)?.ok_or(IoError::Eof)
    }
}

/// Singleton for storing the stdout/stdin handles
///
/// On first access, this also installs the kernel panic hook that informs the
/// kernel that our task has panicked.
static KERN_IO: LazyLock<Mutex<KernIo>> = LazyLock::new(|| {
    install_panic_hook();
    Mutex::new(KernIo {
        reader: BufReader::new(std::io::stdin()),
        writer: std::io::stdout(),
    })
});

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
    // A poisoned lock means a panic in the middle of an exchange, so just hard
    // exit with the EXIT_PANIC return code.
    let mut kernel = KERN_IO
        .lock()
        .unwrap_or_else(|_e| std::process::exit(hostcall::EXIT_PANIC));
    let result = kernel.exchange(&call);
    drop(kernel);
    result.unwrap_or_else(|e| fatal_syscall_error(call.nr, e))
}

/// We had something go terribly wrong while serving a system call, likely
/// during comms with the kernel.
///
/// Prints an error to stderr, and kills the task with a return code that can
/// be caught by the kernel.
fn fatal_syscall_error(what: abi::Sysnum, e: impl std::fmt::Debug) -> ! {
    eprintln!("fatal error during syscall {what}: {e:?}");
    std::process::exit(hostcall::EXIT_TRANSPORT);
}

/// Copy the [`Return`]'s output buffer to `dst`, returning the amount copied.
///
/// If `ret.output` or `dst` are shorter than `len`, the process is aborted.
#[must_use]
fn copy_output_checked(
    what: abi::Sysnum,
    dst: &mut [u8],
    ret: &Return,
    len: u32,
) -> usize {
    let len = len as usize;

    // If the kernel's given output buffer is shorter than the amount to copy,
    // then something has gone wrong.
    let Some(src) = ret.output.get(..len) else {
        fatal_syscall_error(
            what,
            "the kernel returned an output buffer shorter than reported",
        );
    };

    // If the destination buffer is shorter than the amount to copy, then
    // something has gone wrong.
    let Some(dst) = dst.get_mut(..len) else {
        fatal_syscall_error(
            what,
            "the kernel reported more output bytes than the buffer holds",
        );
    };

    dst.copy_from_slice(src);
    len
}

/// Converts a register-returned `u32` to a `TaskId`.
///
/// Aborts the process if the ID is not a valid `u16`.
fn task_id_checked(what: abi::Sysnum, id: u32) -> TaskId {
    let Ok(id) = u16::try_from(id) else {
        fatal_syscall_error(
            what,
            "the kernel returned a task id that isn't 16 bit",
        );
    };
    TaskId(id)
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
                        let rep = &lease._kern_rep;
                        // Safety: built by a Lease constructor.
                        unsafe {
                            core::slice::from_raw_parts(
                                rep.base_address.as_ptr(),
                                rep.length,
                            )
                        }
                        .to_vec()
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
        let reply_len =
            copy_output_checked(abi::Sysnum::Send, incoming, &ret, reply_len);

        for (lease, writeback) in leases.iter().zip(ret.lease_writebacks) {
            let Some(data) = writeback else {
                continue;
            };
            let rep = &lease._kern_rep;

            if !rep.attributes.contains(LeaseAttributes::WRITE) {
                fatal_syscall_error(
                    abi::Sysnum::Send,
                    "the kernel wrote back into a lease that is not writable",
                );
            }

            // Safety: writable leases come from `&mut [u8]`.
            let bytes = unsafe {
                core::slice::from_raw_parts_mut(
                    rep.base_address.as_mut_ptr(),
                    rep.length,
                )
            };

            // The kernel should always send back exactly sized lease writebacks
            if bytes.len() != data.len() {
                fatal_syscall_error(
                    abi::Sysnum::Send,
                    "the kernel sent back a mismatched lease payload",
                );
            }
            bytes.copy_from_slice(&data);
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
                let message_len =
                    copy_output_checked(abi::Sysnum::Recv, buffer, &ret, len);

                Ok(RecvMessage {
                    sender: task_id_checked(abi::Sysnum::Recv, sender),
                    operation,
                    message_len,
                    response_capacity: response_capacity as usize,
                    lease_count: lease_count as usize,
                })
            }
            // The kernel ABI says an open receive cannot fail, and callers rely
            // on that with `unreachable_unchecked`, abort if this condition is
            // violated.
            _ if specific_sender.is_none() => fatal_syscall_error(
                abi::Sysnum::Recv,
                "the kernel failed an open receive, which cannot fail",
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
        (
            code,
            copy_output_checked(
                abi::Sysnum::BorrowRead,
                dest, &ret, num_copied),
        )
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
        task_id_checked(abi::Sysnum::RefreshTaskId, id)
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
