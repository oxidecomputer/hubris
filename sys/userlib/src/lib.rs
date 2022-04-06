// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! User application support library for Hubris.
//!
//! This contains syscall stubs and types, and re-exports the contents of the
//! `abi` crate that gets shared with the kernel.
//!
//! # Syscall stub implementations
//!
//! Each syscall stub consists of two parts: a public `sys_foo` function
//! intended for use by programs, and an internal `sys_foo_stub` function. This
//! might seem like needless duplication, and in a way, it is.
//!
//! Limitations in the behavior of the current `asm!` feature mean we have a
//! hard time moving values into registers r6, r7, and r11. Because (for better
//! or worse) the syscall ABI uses these registers, we have to take extra steps.
//!
//! The `stub` function contains the actual `asm!` call sequence. It is `naked`,
//! meaning the compiler will *not* attempt to do any framepointer/basepointer
//! nonsense, and we can thus reason about the assignment and availability of
//! all registers.
//!
//! See: https://github.com/rust-lang/rust/issues/73450#issuecomment-650463347

#![no_std]
#![feature(asm)]
#![feature(naked_functions)]

#[macro_use]
pub mod macros;

pub use abi::*;
pub use num_derive::{FromPrimitive, ToPrimitive};
pub use num_traits::{FromPrimitive, ToPrimitive};
pub use unwrap_lite::UnwrapLite;

use core::marker::PhantomData;

pub mod hl;
pub mod kipc;
pub mod task_slot;
pub mod units;
pub mod util;

#[derive(Debug)]
#[repr(transparent)]
pub struct Lease<'a> {
    _kern_rep: abi::ULease,
    _marker: PhantomData<&'a mut ()>,
}

impl<'a> Lease<'a> {
    pub fn read_only(x: &'a [u8]) -> Self {
        Self {
            _kern_rep: abi::ULease {
                attributes: abi::LeaseAttributes::READ,
                base_address: x.as_ptr() as u32,
                length: x.len() as u32,
            },
            _marker: PhantomData,
        }
    }

    pub fn read_write(x: &'a mut [u8]) -> Self {
        Self {
            _kern_rep: abi::ULease {
                attributes: LeaseAttributes::READ | LeaseAttributes::WRITE,
                base_address: x.as_ptr() as u32,
                length: x.len() as u32,
            },
            _marker: PhantomData,
        }
    }

    pub fn write_only(x: &'a mut [u8]) -> Self {
        Self {
            _kern_rep: abi::ULease {
                attributes: LeaseAttributes::WRITE,
                base_address: x.as_ptr() as u32,
                length: x.len() as u32,
            },
            _marker: PhantomData,
        }
    }
}

impl<'a> From<&'a [u8]> for Lease<'a> {
    fn from(x: &'a [u8]) -> Self {
        Self::read_only(x)
    }
}

impl<'a> From<&'a mut [u8]> for Lease<'a> {
    fn from(x: &'a mut [u8]) -> Self {
        Self::read_write(x)
    }
}

/// Return type for stubs that return an `(rc, len)` tuple, because the layout
/// of tuples is not specified in the C ABI, and we're using the C ABI to
/// interface to assembler.
///
/// Register-return of structs is also not guaranteed by the C ABI, so we
/// represent the pair of returned registers with something that *can* get
/// passed back in registers: a `u64`.
#[repr(transparent)]
struct RcLen(u64);

impl From<RcLen> for (u32, usize) {
    fn from(s: RcLen) -> Self {
        (s.0 as u32, (s.0 >> 32) as usize)
    }
}

#[inline(always)]
pub fn sys_send(
    target: TaskId,
    operation: u16,
    outgoing: &[u8],
    incoming: &mut [u8],
    leases: &[Lease<'_>],
) -> (u32, usize) {
    let mut args = SendArgs {
        packed_target_operation: u32::from(target.0) << 16
            | u32::from(operation),
        outgoing_ptr: outgoing.as_ptr(),
        outgoing_len: outgoing.len(),
        incoming_ptr: incoming.as_mut_ptr(),
        incoming_len: incoming.len(),
        lease_ptr: leases.as_ptr(),
        lease_len: leases.len(),
    };
    unsafe { sys_send_stub(&mut args).into() }
}

#[allow(dead_code)] // this gets used from asm
#[repr(C)] // field order matters
struct SendArgs<'a> {
    packed_target_operation: u32,
    outgoing_ptr: *const u8,
    outgoing_len: usize,
    incoming_ptr: *mut u8,
    incoming_len: usize,
    lease_ptr: *const Lease<'a>,
    lease_len: usize,
}

/// Core implementation of the SEND syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_send_stub(_args: &mut SendArgs<'_>) -> RcLen {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r7, lr}}
                mov r4, r8
                mov r5, r9
                mov r6, r10
                mov r7, r11
                push {{r4-r7}}
                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4
                @ Load in args from the struct.
                ldm r0!, {{r4-r7}}
                ldm r0, {{r0-r2}}
                mov r8, r0
                mov r9, r1
                mov r10, r2

                @ To the kernel!
                svc #0

                @ Move the two results back into their return positions.
                mov r0, r4
                mov r1, r5
                @ Restore the registers we used.
                pop {{r4-r7}}
                mov r8, r4
                mov r9, r5
                mov r10, r6
                mov r11, r7
                pop {{r4-r7, pc}}
                ",
                sysnum = const Sysnum::Send as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r11}}
                @ Load in args from the struct.
                ldm r0, {{r4-r10}}
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ Move the two results back into their return positions.
                mov r0, r4
                mov r1, r5
                @ Restore the registers we used.
                pop {{r4-r11}}
                @ Fin.
                bx lr
                ",
                sysnum = const Sysnum::Send as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_send_stub for ARM profile");
        }
    }
}

/// Performs an "open" RECV that will accept messages from any task or
/// notifications from the kernel.
///
/// The next message sent to this task, or the highest priority message if
/// several are pending simultaneously, will be written into `buffer`, and its
/// information returned.
///
/// `notification_mask` determines which notification bits can interrupt this
/// RECV (any that are 1). If a notification interrupts the RECV, you will get a
/// "message" originating from `TaskId::KERNEL`.
///
/// This operation cannot fail -- it can be interrupted by a notification if you
/// let it, but it always receives _something_.
#[inline(always)]
pub fn sys_recv_open(buffer: &mut [u8], notification_mask: u32) -> RecvMessage {
    // The open-receive version of the syscall is defined as being unable to
    // fail, and so we should always get a success here. (This is not using
    // `unwrap` because that generates handling code with formatting.)
    match sys_recv(buffer, notification_mask, None) {
        Ok(rm) => rm,
        Err(_) => panic!(),
    }
}

/// Performs a "closed" RECV that will only accept messages from `sender`.
///
/// The next message sent from `sender` to this task (including a message that
/// has already been sent, but is blocked) will be written into `buffer`, and
/// its information returned.
///
/// `notification_mask` determines which notification bits can interrupt this
/// RECV (any that are 1). Note that, if `sender` is not `TaskId::KERNEL`, you
/// can't actually receive any notifications with this operation, so
/// `notification_mask` should always be zero in that case.
///
/// If `sender` is stale (i.e. refers to a deceased generation of the task) when
/// you call this, or if `sender` is rebooted while you're blocked in this
/// operation, this will fail with `ClosedRecvError::Dead`.
#[inline(always)]
pub fn sys_recv_closed(
    buffer: &mut [u8],
    notification_mask: u32,
    sender: TaskId,
) -> Result<RecvMessage, ClosedRecvError> {
    sys_recv(buffer, notification_mask, Some(sender))
        .map_err(|_| ClosedRecvError::Dead)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClosedRecvError {
    Dead,
}

/// General version of RECV that lets you pick closed vs. open receive at
/// runtime.
///
/// You almost always want `sys_recv_open` or `sys_recv_closed` instead.
#[inline(always)]
pub fn sys_recv(
    buffer: &mut [u8],
    notification_mask: u32,
    specific_sender: Option<TaskId>,
) -> Result<RecvMessage, u32> {
    use core::mem::MaybeUninit;

    // Flatten option into a packed u32.
    let specific_sender = specific_sender
        .map(|tid| (1u32 << 31) | u32::from(tid.0))
        .unwrap_or(0);
    let mut out = MaybeUninit::<RawRecvMessage>::uninit();
    let rc = unsafe {
        sys_recv_stub(
            buffer.as_mut_ptr(),
            buffer.len(),
            notification_mask,
            specific_sender,
            out.as_mut_ptr(),
        )
    };

    // Safety: stub fully initializes output struct. On failure, it might
    // initialize it with nonsense, but that's okay -- it's still initialized.
    let out = unsafe { out.assume_init() };

    if rc == 0 {
        Ok(RecvMessage {
            sender: TaskId(out.sender as u16),
            operation: out.operation,
            message_len: out.message_len,
            response_capacity: out.response_capacity,
            lease_count: out.lease_count,
        })
    } else {
        Err(rc)
    }
}

pub struct RecvMessage {
    pub sender: TaskId,
    pub operation: u32,
    pub message_len: usize,
    pub response_capacity: usize,
    pub lease_count: usize,
}

/// Core implementation of the RECV syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
#[must_use]
unsafe extern "C" fn sys_recv_stub(
    _buffer_ptr: *mut u8,
    _buffer_len: usize,
    _notification_mask: u32,
    _specific_sender: u32,
    _out: *mut RawRecvMessage,
) -> u32 {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r7, lr}}
                mov r4, r8
                mov r5, r9
                mov r6, r10
                mov r7, r11
                push {{r4-r7}}
                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4
                @ Move register arguments into their proper positions.
                mov r4, r0
                mov r5, r1
                mov r6, r2
                mov r7, r3
                @ Read output buffer pointer from stack into a register that
                @ is preserved during our syscall. Since we just pushed a
                @ bunch of stuff, we need to read *past* it.
                ldr r3, [sp, #(9 * 4)]

                @ To the kernel!
                svc #0

                @ Move status flag (only used for closed receive) into return
                @ position
                mov r0, r4
                @ Write all the results out into the raw output buffer.
                stm r3!, {{r5-r7}}
                mov r5, r8
                mov r6, r9
                stm r3!, {{r5-r6}}

                @ Restore the registers we used.
                pop {{r4-r7}}
                mov r8, r4
                mov r9, r5
                mov r10, r6
                mov r11, r7
                pop {{r4-r7, pc}}
                ",
                sysnum = const Sysnum::Recv as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r11}}
                @ Move register arguments into their proper positions.
                mov r4, r0
                mov r5, r1
                mov r6, r2
                mov r7, r3
                @ Read output buffer pointer from stack into a register that
                @ is preserved during our syscall. Since we just pushed a
                @ bunch of stuff, we need to read *past* it.
                ldr r3, [sp, #(8 * 4)]
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ Move status flag (only used for closed receive) into return
                @ position
                mov r0, r4
                @ Write all the results out into the raw output buffer.
                stm r3, {{r5-r9}}
                @ Restore the registers we used.
                pop {{r4-r11}}
                @ Fin.
                bx lr
                ",
                sysnum = const Sysnum::Recv as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_recv_stub for ARM profile");
        }
    }
}

/// Duplicated version of `RecvMessage` with all 32-bit fields and predictable
/// field order, so that it can be generated from assembly.
///
/// TODO: might be able to merge this into actual `RecvMessage` with some care.
#[repr(C)]
struct RawRecvMessage {
    pub sender: u32,
    pub operation: u32,
    pub message_len: usize,
    pub response_capacity: usize,
    pub lease_count: usize,
}

#[inline(always)]
pub fn sys_reply(peer: TaskId, code: u32, message: &[u8]) {
    unsafe {
        sys_reply_stub(peer.0 as u32, code, message.as_ptr(), message.len())
    }
}

/// Core implementation of the REPLY syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_reply_stub(
    _peer: u32,
    _code: u32,
    _message_ptr: *const u8,
    _message_len: usize,
) {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff. Note
                @ that we're being clever and pushing only the registers we
                @ need; this means the pop sequence at the end needs to match!
                push {{r4-r7, lr}}
                mov r4, r11
                push {{r4}}

                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4
                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1
                mov r6, r2
                mov r7, r3

                @ To the kernel!
                svc #0

                @ This call has no results.

                @ Restore the registers we used and return.
                pop {{r4}}
                mov r11, r4
                pop {{r4-r7, pc}}
                ",
                sysnum = const Sysnum::Reply as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff. Note
                @ that we're being clever and pushing only the registers we
                @ need; this means the pop sequence at the end needs to match!
                push {{r4-r7, r11, lr}}

                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1
                mov r6, r2
                mov r7, r3
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ This call has no results.

                @ Restore the registers we used and return.
                pop {{r4-r7, r11, pc}}
                ",
                sysnum = const Sysnum::Reply as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_reply_stub for ARM profile");
        }
    }
}

/// Sets this task's timer.
///
/// The timer is set to `deadline`. If `deadline` is `None`, the timer is
/// disabled. Otherwise, the timer is configured to notify when the specified
/// time (in ticks since boot) is reached. When that occurs, the `notifications`
/// will get posted to this task, and the timer will be disabled.
///
/// If the deadline is chosen such that the timer *would have already fired*,
/// had it been set earlier -- that is, if the deadline is `<=` the current time
/// -- the `notifications` will be posted immediately and the timer will not be
/// enabled.
#[inline(always)]
pub fn sys_set_timer(deadline: Option<u64>, notifications: u32) {
    let raw_deadline = deadline.unwrap_or(0);
    unsafe {
        sys_set_timer_stub(
            deadline.is_some() as u32,
            raw_deadline as u32,
            (raw_deadline >> 32) as u32,
            notifications,
        )
    }
}

/// Core implementation of the SET_TIMER syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_set_timer_stub(
    _set_timer: u32,
    _deadline_lo: u32,
    _deadline_hi: u32,
    _notification: u32,
) {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r7, lr}}
                mov r4, r11
                push {{r4}}

                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4
                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1
                mov r6, r2
                mov r7, r3

                @ To the kernel!
                svc #0

                @ This call has no results.

                @ Restore the registers we used and return.
                pop {{r4}}
                mov r11, r4
                pop {{r4-r7, pc}}
                ",
                sysnum = const Sysnum::SetTimer as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r7, r11, lr}}

                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1
                mov r6, r2
                mov r7, r3
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ This call has no results.

                @ Restore the registers we used and return.
                pop {{r4-r7, r11, pc}}
                ",
                sysnum = const Sysnum::SetTimer as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_set_timer_stub for ARM profile")
        }
    }
}

#[inline(always)]
pub fn sys_borrow_read(
    lender: TaskId,
    index: usize,
    offset: usize,
    dest: &mut [u8],
) -> (u32, usize) {
    let mut args = BorrowReadArgs {
        lender: lender.0 as u32,
        index,
        offset,
        dest: dest.as_mut_ptr(),
        dest_len: dest.len(),
    };
    unsafe { sys_borrow_read_stub(&mut args).into() }
}

/// Core implementation of the BORROW_READ syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_borrow_read_stub(_args: *mut BorrowReadArgs) -> RcLen {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r7, lr}}
                mov r4, r8
                mov r5, r11
                push {{r4, r5}}

                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4
                @ Move register arguments into place.
                ldm r0!, {{r4-r7}}
                ldm r0, {{r0}}
                mov r8, r0

                @ To the kernel!
                svc #0

                @ Move the results into place.
                mov r0, r4
                mov r1, r5

                @ Restore the registers we used and return.
                pop {{r4, r5}}
                mov r11, r5
                mov r8, r4
                pop {{r4-r7, pc}}
                ",
                sysnum = const Sysnum::BorrowRead as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r8, r11}}

                @ Move register arguments into place.
                ldm r0, {{r4-r8}}
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ Move the results into place.
                mov r0, r4
                mov r1, r5

                @ Restore the registers we used and return.
                pop {{r4-r8, r11}}
                bx lr
                ",
                sysnum = const Sysnum::BorrowRead as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_borrow_read_stub for ARM profile")
        }
    }
}

#[repr(C)]
struct BorrowReadArgs {
    lender: u32,
    index: usize,
    offset: usize,
    dest: *mut u8,
    dest_len: usize,
}

#[inline(always)]
pub fn sys_borrow_write(
    lender: TaskId,
    index: usize,
    offset: usize,
    src: &[u8],
) -> (u32, usize) {
    let mut args = BorrowWriteArgs {
        lender: lender.0 as u32,
        index,
        offset,
        src: src.as_ptr(),
        src_len: src.len(),
    };
    unsafe { sys_borrow_write_stub(&mut args).into() }
}

/// Core implementation of the BORROW_WRITE syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_borrow_write_stub(
    _args: *mut BorrowWriteArgs,
) -> RcLen {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r7, lr}}
                mov r4, r8
                mov r5, r11
                push {{r4, r5}}

                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4
                @ Move register arguments into place.
                ldm r0!, {{r4-r7}}
                ldr r0, [r0]
                mov r8, r0

                @ To the kernel!
                svc #0

                @ Move the results into place.
                mov r0, r4
                mov r1, r5

                @ Restore the registers we used and return.
                pop {{r4, r5}}
                mov r11, r5
                mov r8, r4
                pop {{r4-r7, pc}}
                bx lr
                ",
                sysnum = const Sysnum::BorrowWrite as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r8, r11}}

                @ Move register arguments into place.
                ldm r0, {{r4-r8}}
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ Move the results into place.
                mov r0, r4
                mov r1, r5

                @ Restore the registers we used and return.
                pop {{r4-r8, r11}}
                bx lr
                ",
                sysnum = const Sysnum::BorrowWrite as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_borrow_write_stub for ARM profile")
        }
    }
}

#[repr(C)]
struct BorrowWriteArgs {
    lender: u32,
    index: usize,
    offset: usize,
    src: *const u8,
    src_len: usize,
}

#[inline(always)]
pub fn sys_borrow_info(lender: TaskId, index: usize) -> Option<BorrowInfo> {
    use core::mem::MaybeUninit;

    let mut raw = MaybeUninit::<RawBorrowInfo>::uninit();
    unsafe {
        sys_borrow_info_stub(lender.0 as u32, index, raw.as_mut_ptr());
    }
    // Safety: stub completely initializes record
    let raw = unsafe { raw.assume_init() };

    if raw.rc == 0 {
        Some(BorrowInfo {
            attributes: abi::LeaseAttributes::from_bits_truncate(raw.atts),
            len: raw.length,
        })
    } else {
        None
    }
}

#[repr(C)]
struct RawBorrowInfo {
    rc: u32,
    atts: u32,
    length: usize,
}

/// Information record returned by `sys_borrow_info`.
pub struct BorrowInfo {
    /// Attributes of the lease.
    pub attributes: abi::LeaseAttributes,
    /// Length of borrowed memory, in bytes.
    pub len: usize,
}

/// Core implementation of the BORROW_INFO syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_borrow_info_stub(
    _lender: u32,
    _index: usize,
    _out: *mut RawBorrowInfo,
) {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r6, lr}}
                mov r4, r11
                push {{r4}}

                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4
                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1

                @ To the kernel!
                svc #0

                @ Move the results into place.
                stm r2!, {{r4-r6}}

                @ Restore the registers we used and return.
                pop {{r4}}
                mov r11, r4
                pop {{r4-r6, pc}}
                ",
                sysnum = const Sysnum::BorrowInfo as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r6, r11}}

                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ Move the results into place.
                stm r2, {{r4-r6}}

                @ Restore the registers we used and return.
                pop {{r4-r6, r11}}
                bx lr
                ",
                sysnum = const Sysnum::BorrowInfo as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_borrow_write_stub for ARM profile")
        }
    }
}

#[inline(always)]
pub fn sys_irq_control(mask: u32, enable: bool) {
    unsafe {
        sys_irq_control_stub(mask, enable as u32);
    }
}

/// Core implementation of the IRQ_CONTROL syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_irq_control_stub(_mask: u32, _enable: u32) {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4, r5, lr}}
                mov r4, r11
                push {{r4}}

                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4
                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1

                @ To the kernel!
                svc #0

                @ This call returns no results.

                @ Restore the registers we used and return.
                pop {{r4}}
                mov r11, r4
                pop {{r4, r5, pc}}
                ",
                sysnum = const Sysnum::IrqControl as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4, r5, r11, lr}}

                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ This call returns no results.

                @ Restore the registers we used and return.
                pop {{r4, r5, r11, pc}}
                ",
                sysnum = const Sysnum::IrqControl as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_irq_control stub for ARM profile")
        }
    }
}

#[inline(always)]
pub fn sys_panic(msg: &[u8]) -> ! {
    unsafe { sys_panic_stub(msg.as_ptr(), msg.len()) }
}

/// Core implementation of the PANIC syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_panic_stub(_msg: *const u8, _len: usize) -> ! {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ We're not going to return, so technically speaking we don't
                @ need to save registers. However, we save them anyway, so that
                @ we can reconstruct the state that led to the panic.
                push {{r4, r5, lr}}
                mov r4, r11
                push {{r4}}

                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4
                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1

                @ To the kernel!
                svc #0
                @ noreturn generates a udf to trap us if it returns.
                ",
                sysnum = const Sysnum::Panic as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ We're not going to return, so technically speaking we don't
                @ need to save registers. However, we save them anyway, so that
                @ we can reconstruct the state that led to the panic.
                push {{r4, r5, r11, lr}}

                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0
                @ noreturn generates a udf to trap us if it returns.
                ",
                sysnum = const Sysnum::Panic as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_panic_stub for ARM profile")
        }
    }
}

/// Reads the state of this task's timer.
///
/// This returns three values in a `TimerState` struct:
///
/// - `now` is the current time on the timer, in ticks since boot.
/// - `deadline` is either `None`, meaning the timer notifications are disabled,
///   or `Some(t)`, meaning the timer will post notifications at time `t`.
/// - `on_dl` are the notification bits that will be posted on deadline.
///
/// `deadline` and `on_dl` are as configured by `sys_set_timer`.
///
/// `now` is monotonically advancing and can't be changed.
#[inline(always)]
pub fn sys_get_timer() -> TimerState {
    use core::mem::MaybeUninit;

    let mut out = MaybeUninit::<RawTimerState>::uninit();
    unsafe {
        sys_get_timer_stub(out.as_mut_ptr());
    }
    // Safety: stub fully initializes output struct.
    let out = unsafe { out.assume_init() };

    TimerState {
        now: u64::from(out.now_lo) | u64::from(out.now_hi) << 32,
        deadline: if out.set != 0 {
            Some(u64::from(out.dl_lo) | u64::from(out.dl_hi) << 32)
        } else {
            None
        },
        on_dl: out.on_dl,
    }
}

/// Result of `sys_get_timer`, provides information about task timer state.
pub struct TimerState {
    /// Current task timer time, in ticks.
    pub now: u64,
    /// Current deadline, or `None` if the deadline is not pending.
    pub deadline: Option<u64>,
    /// Notifications to be delivered if the deadline is reached.
    pub on_dl: u32,
}

#[repr(C)] // loaded from assembly, field order must not change
struct RawTimerState {
    now_lo: u32,
    now_hi: u32,
    set: u32,
    dl_lo: u32,
    dl_hi: u32,
    on_dl: u32,
}

/// Core implementation of the GET_TIMER syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_get_timer_stub(_out: *mut RawTimerState) {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r7, lr}}
                mov r4, r8
                mov r5, r9
                mov r6, r10
                mov r7, r11
                push {{r4-r7}}
                @ Load the constant syscall number.
                eors r4, r4
                adds r4, #{sysnum}
                mov r11, r4

                @ To the kernel!
                svc #0

                @ Write all the results out into the raw output buffer.
                stm r0!, {{r4-r7}}
                mov r4, r8
                mov r5, r9
                stm r0!, {{r4, r5}}
                @ Restore the registers we used.
                pop {{r4-r7}}
                mov r11, r7
                mov r10, r6
                mov r9, r5
                mov r8, r4
                pop {{r4-r7, pc}}
                ",
                sysnum = const Sysnum::GetTimer as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4-r11}}
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ Write all the results out into the raw output buffer.
                stm r0, {{r4-r9}}
                @ Restore the registers we used.
                pop {{r4-r11}}
                @ Fin.
                bx lr
                ",
                sysnum = const Sysnum::GetTimer as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_get_timer_stub for ARM profile")
        }
    }
}

/// This is the entry point for the task, invoked by the kernel. Its job is to
/// set up our memory before jumping to user-defined `main`.
#[doc(hidden)]
#[no_mangle]
#[link_section = ".text.start"]
#[naked]
pub unsafe extern "C" fn _start() -> ! {
    // Provided by the user program:
    extern "Rust" {
        fn main() -> !;
    }

    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Copy data initialization image into data section.
                @ Note: this assumes that both source and destination are 32-bit
                @ aligned and padded to 4-byte boundary.

                ldr r0, =__edata            @ upper bound in r0
                ldr r1, =__sidata           @ source in r1
                ldr r2, =__sdata            @ dest in r2

                b 1f                        @ check for zero-sized data

            2:  ldm r1!, {{r3}}             @ read and advance source
                stm r2!, {{r3}}             @ write and advance dest

            1:  cmp r2, r0                  @ has dest reached the upper bound?
                bne 2b                      @ if not, repeat

                @ Zero BSS section.

                ldr r0, =__ebss             @ upper bound in r0
                ldr r1, =__sbss             @ base in r1

                movs r2, #0                 @ materialize a zero

                b 1f                        @ check for zero-sized BSS

            2:  stm r1!, {{r2}}             @ zero one word and advance

            1:  cmp r1, r0                  @ has base reached bound?
                bne 2b                      @ if not, repeat

                @ Be extra careful to ensure that those side effects are
                @ visible to the user program.

                dsb         @ complete all writes
                isb         @ and flush the pipeline

                @ Now, to the user entry point. We call it in case it
                @ returns. (It's not supposed to.) We reference it through
                @ a sym operand because it's a Rust func and may be mangled.
                bl {main}

                @ The noreturn option below will automatically generate an
                @ undefined instruction trap past this point, should main
                @ return.
                ",
                main = sym main,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Copy data initialization image into data section.
                @ Note: this assumes that both source and destination are 32-bit
                @ aligned and padded to 4-byte boundary.

                movw r0, #:lower16:__edata  @ upper bound in r0
                movt r0, #:upper16:__edata

                movw r1, #:lower16:__sidata @ source in r1
                movt r1, #:upper16:__sidata

                movw r2, #:lower16:__sdata  @ dest in r2
                movt r2, #:upper16:__sdata

                b 1f                        @ check for zero-sized data

            2:  ldr r3, [r1], #4            @ read and advance source
                str r3, [r2], #4            @ write and advance dest

            1:  cmp r2, r0                  @ has dest reached the upper bound?
                bne 2b                      @ if not, repeat

                @ Zero BSS section.

                movw r0, #:lower16:__ebss   @ upper bound in r0
                movt r0, #:upper16:__ebss

                movw r1, #:lower16:__sbss   @ base in r1
                movt r1, #:upper16:__sbss

                movs r2, #0                 @ materialize a zero

                b 1f                        @ check for zero-sized BSS

            2:  str r2, [r1], #4            @ zero one word and advance

            1:  cmp r1, r0                  @ has base reached bound?
                bne 2b                      @ if not, repeat

                @ Be extra careful to ensure that those side effects are
                @ visible to the user program.

                dsb         @ complete all writes
                isb         @ and flush the pipeline

                @ Now, to the user entry point. We call it in case it
                @ returns. (It's not supposed to.) We reference it through
                @ a sym operand because it's a Rust func and may be mangled.
                bl {main}

                @ The noreturn option below will automatically generate an
                @ undefined instruction trap past this point, should main
                @ return.
                ",
                main = sym main,
                options(noreturn),
            )
        } else {
            compiler_error!("missing .start routine for ARM profile")
        }
    }
}

/// Panic handler for user tasks with the `panic-messages` feature enabled. This
/// handler will try its best to generate a panic message, up to a maximum
/// buffer size (configured below).
///
/// Including this panic handler permanently reserves a buffer in the RAM of a
/// task, to ensure that memory is available for the panic message, even if the
/// resources have been trimmed aggressively using `xtask sizes` and `humility
/// stackmargin`.
#[cfg(feature = "panic-messages")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Implementation Note
    //
    // This is a panic handler (obvs). Panic handlers have a unique
    // responsibility: they are the only piece of Rust code that _must not
    // panic._ (If they do, they wind up calling themselves recursively, which
    // usually translates a simple panic into a harder-to-diagnose stack
    // overflow.)
    //
    // There is unfortunately no way to have the compiler _check_ that the code
    // does not panic, so we have to work very carefully.

    // There's a tradeoff here between "getting a useful message" and "wasting a
    // lot of RAM." Somewhat arbitrarily, we choose to collect this many bytes
    // of panic message (and permanently reserve the same number of bytes of
    // RAM):
    const BUFSIZE: usize = 128;

    // Panic messages get constructed using `core::fmt::Write`. If we implement
    // that trait, we can provide our own type that will back the
    // `core::fmt::Formatter` handed into any formatting routines (like those on
    // `Debug`/`Display`). This is important, because the standard library does
    // not provide an implementation of `core::fmt::Write` that uses a fixed
    // size buffer and discards the rest -- which is what we want.
    //
    // And so, we provide our own!
    use core::fmt::Write;

    struct PrefixWrite {
        /// Content will be written here. While the content itself will be
        /// UTF-8, it may end in an incomplete UTF-8 character to simplify our
        /// truncation logic.
        buf: &'static mut [u8; BUFSIZE],
        /// Number of bytes of `buf` that are valid.
        ///
        /// Invariant: always in the range `0..buf.len()`.
        pos: usize,
    }

    impl Write for PrefixWrite {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            // If we've filled the buffer, this is a no-op.
            //
            // Note that we check `>=` here even though our invariant on this
            // type states that the `>` part won't happen. This is to reassure
            // the compiler, which otherwise can't see our invariant.
            if self.pos >= self.buf.len() {
                return Ok(());
            }

            // We can write more. How much more? Let's find out. We're going to
            // do this using unchecked operations to avoid a recursive panic.
            //
            // Safety: this is unsafe because of the risk of passing in an
            // out-of-bounds index for the split. However, our type invariant
            // ensures that `self.pos` is in-bounds.
            let remaining = unsafe { self.buf.get_unchecked_mut(self.pos..) };
            // We will copy bytes from the input string `s` into `remaining`,
            // using the length of whichever is _shorter_ to stay in bounds.
            let strbytes = s.as_bytes();
            let to_write = usize::min(remaining.len(), strbytes.len());
            // Copy!
            //
            // Safety: to use this copy operation safely we must ensure the
            // following.
            //
            // - The source pointer must be readable for the given number of
            //   bytes, which we ensure by taking it from the `strbytes` slice,
            //   whose length is _at least_ as long as our count (by
            //   construction).
            // - The dest pointer must be writable for the given number of
            //   bytes, which we ensure the same way except by using
            //   `remaining`.
            // - Both pointers must be properly aligned, which is ensured by
            //   getting them from slices.
            // - The affected regions starting at the source and dest pointers
            //   must not overlap, which we can prove trivially since they're
            //   coming from a `&[u8]` and a `&mut [u8]`, which cannot overlap
            //   due to `&mut` aliasing requirements.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    strbytes.as_ptr(),
                    remaining.as_mut_ptr(),
                    to_write,
                );
            }
            // Finally, update our `pos` to record the number of bytes written.
            //
            // We use a wrapping add here to avoid integer overflow checks that
            // could recursively panic. However, we also need to maintain our
            // type invariant that `pos` does not exceed `buf.len()`. We can be
            // sure of this here because:
            //
            // - `remaining.len()` is exactly equal to `buf.len() - pos`.
            // - `to_write` is no larger than `remaining.len()`.
            // - Therefore `pos + to_write <= buf.len()`.
            //
            // While this code is not `unsafe`, violating this invariant would
            // break the `unsafe` code above, hence the size of this comment on
            // what is otherwise a single instruction.
            self.pos = self.pos.wrapping_add(to_write);

            Ok(())
        }
    }

    // We declare a single static panic buffer per task, to ensure the memory is
    // available.
    static mut PANIC_BUFFER: [u8; BUFSIZE] = [0; BUFSIZE];

    // Okay. Now we start the actual panicking process.
    //
    // Safety: this is unsafe because we're getting a reference to a static mut,
    // and the compiler can't be sure we're not aliasing or racing. We can be
    // sure that this reference won't be aliased elsewhere in the program,
    // because we've lexically confined it to this block.
    //
    // However, it is possible to produce an alias if the panic handler is
    // called reentrantly. This can only happen if the code in the panic handler
    // itself panics, which is what we're working very hard to prevent here.
    let panic_buffer = unsafe { &mut PANIC_BUFFER };

    // Whew! Time to write the darn message.
    //
    // Note that if we provided a different value of `pos` here we could destroy
    // PrefixWrite's type invariant, so, don't do that.
    let mut pw = PrefixWrite {
        buf: panic_buffer,
        pos: 0,
    };
    write!(pw, "{}", info).ok();

    // Get the written part of the message.
    //
    // Safety: this is unsafe due to the potential for an out-of-bounds index,
    // but PrefixWrite ensures that `pos <= buf.len()`, so this is ok.
    let msg = unsafe { pw.buf.get_unchecked(..pw.pos) };

    // Pass it to kernel.
    sys_panic(msg)
}

/// Panic handler for tasks without the `panic-messages` feature enabled. This
/// kills the task with a fixed message, `"PANIC"`. While this is less helpful
/// than a proper panic message, the stack trace can still be informative.
#[cfg(not(feature = "panic-messages"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    sys_panic(b"PANIC")
}

#[inline(always)]
pub fn sys_refresh_task_id(task_id: TaskId) -> TaskId {
    let tid = unsafe { sys_refresh_task_id_stub(task_id.0 as u32) };
    TaskId(tid as u16)
}

/// Core implementation of the REFRESH_TASK_ID syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_refresh_task_id_stub(_tid: u32) -> u32 {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                @ match!
                push {{r4, r5, lr}}
                mov r4, r11
                push {{r4}}

                @ Load the constant syscall number.
                movs r4, #0
                adds r4, #{sysnum}
                mov r11, r4

                @ Move register arguments into place.
                mov r4, r0

                @ To the kernel!
                svc #0

                @ Move result into place.
                mov r0, r4

                @ Restore the registers we used and return.
                pop {{r4}}
                mov r11, r4
                pop {{r4, r5, pc}}
                ",
                sysnum = const Sysnum::RefreshTaskId as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4, r5, r11, lr}}

                @ Move register arguments into place.
                mov r4, r0
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ Move result into place.
                mov r0, r4

                @ Restore the registers we used and return.
                pop {{r4, r5, r11, pc}}
                ",
                sysnum = const Sysnum::RefreshTaskId as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_refresh_task_id stub for ARM profile")
        }
    }
}

#[inline(always)]
pub fn sys_post(task_id: TaskId, bits: u32) -> u32 {
    unsafe { sys_post_stub(task_id.0 as u32, bits) }
}

/// Core implementation of the POST syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_post_stub(_tid: u32, _mask: u32) -> u32 {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4, r5, lr}}
                mov r4, r11
                push {{r4}}

                @ Load the constant syscall number.
                movs r4, #0
                adds r4, #{sysnum}
                mov r11, r4

                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1

                @ To the kernel!
                svc #0

                @ Move result into place.
                mov r0, r4

                @ Restore the registers we used and return.
                pop {{r4}}
                mov r11, r4
                pop {{r4, r5, pc}}
                ",
                sysnum = const Sysnum::Post as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4, r5, r11, lr}}

                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ Move result into place.
                mov r0, r4

                @ Restore the registers we used and return.
                pop {{r4, r5, r11, pc}}
                ",
                sysnum = const Sysnum::Post as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_post_stub for ARM profile")
        }
    }
}

#[inline(always)]
pub fn sys_reply_fault(task_id: TaskId, reason: ReplyFaultReason) {
    unsafe { sys_reply_fault_stub(task_id.0 as u32, reason as u32) }
}

/// Core implementation of the REPLY_FAULT syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[naked]
unsafe extern "C" fn sys_reply_fault_stub(_tid: u32, _reason: u32) {
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4, r5, lr}}
                mov r4, r11
                push {{r4}}

                @ Load the constant syscall number.
                movs r4, #0
                adds r4, #{sysnum}
                mov r11, r4
                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1

                @ To the kernel!
                svc #0

                @ This syscall has no results.

                @ Restore the registers we used and return.
                pop {{r4}}
                mov r11, r4
                pop {{r4, r5, pc}}
                ",
                sysnum = const Sysnum::ReplyFault as u32,
                options(noreturn),
            )
        } else if #[cfg(any(armv7m, armv8m))] {
            asm!("
                @ Spill the registers we're about to use to pass stuff.
                push {{r4, r5, r11, lr}}

                @ Move register arguments into place.
                mov r4, r0
                mov r5, r1
                @ Load the constant syscall number.
                mov r11, {sysnum}

                @ To the kernel!
                svc #0

                @ This syscall has no results.

                @ Restore the registers we used and return.
                pop {{r4, r5, r11, pc}}
                ",
                sysnum = const Sysnum::ReplyFault as u32,
                options(noreturn),
            )
        } else {
            compiler_error!("missing sys_reply_fault_stub for ARM profile")
        }
    }
}
