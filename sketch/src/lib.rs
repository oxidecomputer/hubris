//! Proposed/stubbed syscall and IPC interface for Hubris.
//!
//! This interface is designed to make as much interaction as possible safe (in
//! the Rust sense) while being robust against arbitrary unsafe shenanigans and
//! potentially allowing tasks written in C or assembler. This means:
//!
//! - Task-kernel and task-task (IPC) structures need to be predictable layout,
//!   which in practice means `repr(C)` or `repr(transparent)`.
//!
//! - Pointers to data structures can have an assumed length when talking to the
//!   kernel, but everything else needs to carry an explicit length.
//!
//! - The basic IPC operations work in terms of untyped byte slices because
//!   doing anything more complex has some safety implications.

#![no_std]
#![feature(llvm_asm)]

use core::marker::PhantomData;

use bitflags::bitflags;

/// SEND syscall. Transfers a variable-size message to another task and
/// waits for a (variable-size) reply.
///
/// `task` is the ID of the intended recipient.
///
/// `operation` is an uninterpreted 16 bit code passed verbatim to the called
/// task. This is intended to distinguish request types in user APIs.
///
/// `outgoing` describes the message to send.
///
/// `incoming` describes the place to write a response.
///
/// `leases` points to the base of the lease table.
///
/// # Return value
///
/// Returns a `u32` response code and a `usize` response length.
///
/// By convention, a response code of `0` means `Ok` and a non-zero means `Err`.
///
/// A message can be delivered either way, so errors are not restricted to 32
/// bits. The response length measures the length of the prefix of `incoming`
/// that was written.
///
/// # Safety
///
/// This call is safe, under the same caveats as any Rust function (i.e. don't
/// manufacture bogus slices using `unsafe` and we'll all be fine).
///
/// The kernel will check the extents of the slices before using them. If you
/// attempt to pass references to memory your task doesn't own, you won't even
/// get an error back -- your task will take a fault and be dealt with.
pub fn sys_send(
    task: TaskName,
    operation: u16,
    outgoing: &[u8],
    incoming: &mut [u8],
    leases: &[Lease<'_>],
) -> (u32, usize) {
    // We're just almost out of registers here, so pack the two small values
    // into one.
    let operation_and_task = u32::from(task.0) << 16 | u32::from(operation);

    let mut response_code;
    let mut response_len;
    // Safety: the kernel is careful not to violate our safety across this call.
    // Other than calling SVC we're not doing anything unsafe here. The "memory"
    // clobber captures the fact that the contents of `incoming` may change out
    // from under us; it's technically overbroad, but it's not at all clear how
    // to designate a slice as a memory clobber operand.
    unsafe {
        llvm_asm! {
            "svc 0"
            : "={r4}"(response_code),
              "={r5}"(response_len)
            : "{r4}"(operation_and_task),
              "{r5}"(outgoing.as_ptr()),
              "{r6}"(outgoing.len()),
              "{r7}"(incoming.as_ptr()),
              "{r8}"(incoming.len()),
              "{r9}"(leases.as_ptr()),
              "{r10}"(leases.len())
            : "memory"
            : "volatile"
        };
    }
    (response_code, response_len)
}

/// RECEIVE syscall. Blocks waiting for an incoming message, including a
/// notification.
///
/// `buffer` is the place in memory where any incoming message's payload should
/// be deposited.
///
/// Returns a `ReceivedMessage` struct containing additional details about the
/// message.
pub fn sys_receive(buffer: &mut [u8]) -> ReceivedMessage {
    let mut sender: TaskName;
    let mut operation: u16;
    let mut message_len: usize;
    let mut response_capacity: usize;
    let mut lease_count: usize;

    // Safety: this call will, *if it returns*, deposit something in `buffer`
    // and set some registers. So from the perspective of this process, it is
    // safe. (If it does not return, it's also, by definition, safe.)
    unsafe {
        llvm_asm! {
            "svc 1"
            : "={r4}"(sender),
              "={r5}"(operation),
              "={r6}"(message_len),
              "={r7}"(response_capacity),
              "={r8}"(lease_count)
            : "{r4}"(buffer.as_mut_ptr()),
              "{r5}"(buffer.len())
            : "memory"
            : "volatile"
        }
    }
    ReceivedMessage {
        sender,
        operation,
        message_len,
        response_capacity,
        lease_count,
    }
}

/// REPLY syscall. Sends a response to a task that is blocked after SENDing
/// to us.
///
/// `task` names the caller.
///
/// `code` is a 32-bit return code to send verbatim to the task. By
/// convention, 0 means success (`Ok`) and non-zero means `Err`.
///
/// `message` is the message to deliver.
pub fn sys_reply(task: TaskName, code: u32, message: &[u8]) {
    // Safety: from the caller's perspective, `reply` is equivalent to reading
    // the contents of `message`. It has no other user-visible side effects
    // (other than sending a message, obvs)
    unsafe {
        llvm_asm! {
            "svc 3"
            : // no outputs
            : "{r4}"(task),
              "{r5}"(code),
              "{r6}"(message.as_ptr()),
              "{r7}"(message.len())
            : // does not write memory
            : "volatile"
        }
    }
}

/// NOTMASK syscall. Alters the caller's notification mask.
///
/// The notification mask will be computed as follows:
///
/// `new_mask = (old_mask & and) | or;`
pub fn sys_notmask(and: u32, or: u32) {
    // Safety: this call just updates settings in the kernel.
    unsafe {
        llvm_asm! {
            "svc 4"
            : // no outputs
            : "{r4}"(and), "{r5}"(or)
            : // does not write memory
            : "volatile"
        }
    }
}

/// BORROW_INFO syscall. Retrieves information about a borrow.
///
/// `task` is the owner/originator of the lease.
///
/// `index` is the index of the lease within the owner's table.
///
/// # Return value
///
/// Returns a packed `Result`:
/// - Success/failure code (bits `31:0`): 0 for success, 1 for borrow number
///   out of range for lease table.
/// - Size (bits `63:32`): size of borrow on success, zero otherwise.
pub fn sys_borrow_info(
    task: TaskName,
    index: usize,
) -> Result<BorrowInfo, BorrowError> {
    // Safety: this just pulls information from the kernel; it's unsafe simply
    // because we need llvm_asm!.
    let (mut status, mut attributes, mut size);
    unsafe {
        llvm_asm! {
            "svc 5"
            : "={r4}"(status),
              "={r5}"(attributes),
              "={r6}"(size)
            : "{r4}"(task),
              "{r5}"(index)
            : // does not write memory
            : "volatile"
        }
    }
    match status {
        0 => Ok(BorrowInfo { attributes, size }),
        1 => Err(BorrowError::BadIndex),
        2 => Err(BorrowError::WentAway),
        3 => Err(BorrowError::BadOffset), // lol what?
        _ => Err(BorrowError::ReadOnly),  // lol what?
    }
}

/// BORROW_WRITE syscall. Copies bytes from our address space into a
/// writable borrow from another task.
///
/// `control` is the borrow operation control word; see `BORROW_SIZE` above
/// for its encoding.
///
/// # Return value
///
/// Success/failure code:0`): 0 for success, 1 for borrow number out of
/// range for lease table, 2 for out-of-range, 3 for read-only.
pub fn sys_borrow_write(
    task: TaskName,
    index: usize,
    offset: usize,
    data: &[u8],
) -> Result<(), BorrowError> {
    // Safety: this just reads `data`, it's unsafe only because we need llvm_asm!.
    let mut status: u32;
    unsafe {
        llvm_asm! {
            "svc 6"
            : "={r4}"(status)
            : "{r4}"(task),
              "{r5}"(index),
              "{r6}"(offset),
              "{r7}"(data.as_ptr()),
              "{r8}"(data.len())
            : // does not write memory
            : "volatile"
        }
    }
    match status {
        0 => Ok(()),
        1 => Err(BorrowError::BadIndex),
        2 => Err(BorrowError::WentAway),
        3 => Err(BorrowError::BadOffset),
        _ => Err(BorrowError::ReadOnly),
    }
}

/// A type for designating a task you want to interact with.
#[derive(Copy, Clone, uDebug, PartialEq, Eq)]
#[repr(transparent)]
pub struct TaskName(pub u16);

/// Response code returned by the kernel when an IPC involves a dead task.
pub const NO_PEER: u32 = !0;

/// A `Lease` represents something in memory that can be lent to another task.
/// It is equivalent to a Rust reference, in that it represents a borrow of some
/// value and is considered by the borrow checker. It differs from a Rust
/// reference in that (1) it is shared with another task, and (2) it can't
/// actually be used locally to access the data. (Because you don't need it,
/// because you already have the data.)
///
/// A `Lease` is not simply represented as a Rust reference because the kernel
/// needs to be able to inspect it and determine your intent. In particular, it
/// needs to know (1) whether this lease permits the borrower to read, write, or
/// both, and (2) the size of the type or area being lent, as the kernel knows
/// nothing of your "type system."
///
/// Create a `Lease` using `Lease::read` or `Lease::write`. The expectation is
/// that these will be generated immediately before a `send` by some sort of
/// wrapper function, rather than manipulated by normal user code directly.
#[derive(uDebug)]
#[repr(C)]
pub struct Lease<'a> {
    /// Encoding of the lease's properties into a 32-bit word.
    attributes: LeaseAttributes,
    /// Beginning of leased area. Note that this is a `*const` even if the lease
    /// is writable. This is okay, as we don't write *through the lease*, but
    /// let the kernel do it.
    base: *const u8,
    /// Length of leased area.
    len: usize,

    _phantom: PhantomData<&'a mut ()>,
}

impl<'a> Lease<'a> {
    /// Creates a read-only lease on the data in `slice`. If passed to the
    /// kernel with a `send` operation, this lease will enable the target of the
    /// send to read from `slice` until it replies.
    pub fn read(slice: &'a [u8]) -> Self {
        Self {
            attributes: LeaseAttributes::READ,
            base: slice.as_ptr(),
            len: slice.len(),

            _phantom: PhantomData,
        }
    }

    /// Creates a write-only lease on the data in `slice`. If passed to the
    /// kernel with a `send` operation, this lease will enable the target of the
    /// send to write to (but not read from!) `slice` until it replies.
    pub fn write(slice: &'a mut [u8]) -> Self {
        Self {
            attributes: LeaseAttributes::WRITE,
            base: slice.as_ptr(),
            len: slice.len(),

            _phantom: PhantomData,
        }
    }

    /// Creates a read-write lease on the data in `slice`. If passed to the
    /// kernel with a `send` operation, this lease will enable the target of the
    /// send to write to or read from `slice` until it replies.
    pub fn read_write(slice: &'a mut [u8]) -> Self {
        Self {
            attributes: LeaseAttributes::READ | LeaseAttributes::WRITE,
            base: slice.as_ptr(),
            len: slice.len(),

            _phantom: PhantomData,
        }
    }
}

bitflags! {
    /// Internal storage for attributes of leases, to be consumed by the kernel.
    #[repr(transparent)]
    pub struct LeaseAttributes: u32 {
        /// Peer may read from the leased memory.
        const READ = 1 << 0;
        /// Peer may write to the leased memory.
        const WRITE = 1 << 1;
    }
}

/// Information about a message from `receive`, filled in by the kernel.
#[repr(C)]
pub struct ReceivedMessage {
    /// Designates the sender. Normally, this is an application task, and the
    /// kernel guarantees that it is now blocked waiting for your `reply`.
    /// However, this may also be the reserved kernel task ID 0, in which case
    /// this message is actually a notification delivery.
    pub sender: TaskName,
    /// Operation code sent by the sender, verbatim.
    pub operation: u16,
    /// Number of bytes sent.
    ///
    /// **Note:** This number may be *larger* than the length of the buffer you
    /// provided! It is the actual number of bytes provided by the sender. The
    /// actual number of bytes *received* is given by
    /// `usize::min(message_buffer.len(), received_message.message_len)`.
    ///
    /// The expectation is that this subtlety will normally be hidden behind a
    /// wrapper function for the particular API being implemented.
    pub message_len: usize,
    /// Number of bytes the caller has made available to receive your eventual
    /// reply.
    pub response_capacity: usize,
    /// Number of leases the caller has made available.
    pub lease_count: usize,
}

/// Sets the current task's notification mask. 0 bits are disabled (masked), 1
/// bits are enabled (unmasked).
pub fn set_notification_mask(mask: u32) {
    sys_notmask(0, mask)
}

/// Unmasks any notifications corresponding to 1 bits in the parameter. This
/// just ORs the parameter into the notification mask.
pub fn unmask_notifications(mask: u32) {
    sys_notmask(!0, mask)
}

/// Masks any notifications corresponding to 1 bits in the parameter. This ANDs
/// the complement into the notification mask.
pub fn mask_notifications(mask: u32) {
    sys_notmask(!mask, 0)
}

/// Enables the hardware interrupts corresponding to the given notification
/// bits.
///
/// Tasks do not deal in physical IRQ numbers. Instead, hardware interrupts are
/// routed to their notification bits. A task can then request that the hardware
/// interrupts associated with a subset of notification bits (given by 1 bits in
/// the mask here) be enabled.
///
/// If any of the bits given here do not *really* correspond to interrupts, they
/// are ignored. This might become a fault in the future, but ignoring seems
/// like the right choice for testing.
pub fn enable_interrupts(mask: u32) {
    // This assumes that the enable interrupts operation is implemented as a
    // message to the kernel "task" instead of a syscall. We `unwrap` the result
    // because, if the kernel returns an error, something is SERIOUSLY WRONG.
    let (code, _len) = sys_send(
        TaskName(0),
        KernelOps::EnableInterrupts as u16,
        &mask.to_le_bytes(),
        &mut [],
        &[],
    );
    // Kernel sanity checking, probably not necessary
    assert!(code == 0);
}

#[repr(u16)]
pub enum KernelOps {
    EnableInterrupts = 1,
}

pub enum BorrowError {
    BadIndex = 1,
    WentAway = 2,
    BadOffset = 3,
    ReadOnly = 4,
}

pub struct BorrowInfo {
    pub attributes: LeaseAttributes,
    pub size: usize,
}
