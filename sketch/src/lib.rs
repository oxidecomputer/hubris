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

use core::marker::PhantomData;
use core::mem::MaybeUninit;

use bitflags::bitflags;

// Our assembly language entry points
extern "C" {
    fn _sys_send(
        operation_and_task: u32,
        lengths: u32,
        outgoing: *const u8,
        incoming: *mut u8,
        leases: *const Lease<'_>,
    ) -> u64;
    fn _sys_receive(buffer: *mut u8, buffer_len: usize, rxinfo: *mut ReceivedMessage);
    fn _sys_reply(task: TaskName, code: u32, message: *const u8, len: usize);
    fn _sys_notmask(and: u32, or: u32);
}

/// A type for designating a task you want to interact with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct TaskName(pub u16);

pub const NO_PEER: u32 = !0;

/// Sends a message and waits for a reply.
/// 
/// The target task is named by `dest`. If `dest` is a name that is stale (i.e.
/// the target has reset since we last interacted), this returns code `!0`, also
/// known as `NO_PEER`. This will also occur if the peer dies *after* receiving
/// the message but *before* replying.
///
/// The operation being requested is given by the 16-bit code `operation`.
/// Operation codes are application-defined, except when talking to the kernel
/// task, when they are defined by the `KernelOps` enum.
///
/// The request to transmit is identified by `request`. The contents of the
/// slice will be transferred by the kernel into a place defined by the
/// recipient if/when the message is delivered. If the recipient hasn't given
/// enough room for `request` in its entirety, you will not be informed of this,
/// but the recipient will.
///
/// `response` gives the buffer in which the response message, if any, should be
/// written. The message will be written if (1) this message is received and (2)
/// the recipient replies to us. The response will fit in your buffer; if the
/// peer tries to deliver an over-large response, it will be faulted and you'll
/// get `NO_PEER` and a zero-length response instead.
///
/// The `leases` table optionally makes sections of your address space visible
/// to the peer without additional copies. Leases are revoked before this
/// returns, so it's equivalent to borrowing.
///
/// # Return values
///
/// This always returns a pair of `(response_code, reply_length)`.
///
/// `response_code` will be the special value `NO_PEER` (`!0`) if the peer died
/// before the reply was delivered (or earlier). Otherwise, it will be the code
/// sent by the peer. By convention, `0` means success.
///
/// A reply message can be received in either success or failure cases. In the
/// case of a kernel-generated `NO_PEER` the reply length will be zero.
///
/// # Limits
///
/// Both `request` and `response` are limited to 256 bytes, to reduce time spent
/// in the kernel. If you want to send or receive something larger than that,
/// use a `Lease`.
///
/// The `leases` table is limited to 256 entries.
///
/// Violating any of these limits will cause a panic.
pub fn send_untyped(
    dest: TaskName,
    operation: u16,
    request: &[u8],
    response: &mut [u8],
    leases: &[Lease<'_>],
) -> (u32, usize) {
    assert!(request.len() < 0x100);
    assert!(response.len() < 0x100);
    assert!(leases.len() < 0x100);

    let combined_lengths = request.len() as u32
        | (response.len() as u32) << 8
        | (leases.len() as u32) << 16;

    let packed_code_len = unsafe {
        _sys_send(
            u32::from(operation) | u32::from(dest.0) << 16,
            combined_lengths,
            request.as_ptr(),
            response.as_mut_ptr(),
            leases.as_ptr(),
        )
    };
    (packed_code_len as u32, (packed_code_len >> 32) as usize)
}

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
#[derive(Debug)]
#[repr(C)]
pub struct Lease<'a> {
    attributes: LeaseAttributes,
    base: *const u8,
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
    struct LeaseAttributes: u32 {
        /// Peer may read from the leased memory.
        const READ = 1 << 0;
        /// Peer may write to the leased memory.
        const WRITE = 1 << 1;
    }
}

/// Receives the highest priority incoming message from any source.
///
/// Messages will be preempted if your task has any posted, unmasked
/// notifications. To prevent this behavior, set your notification mask.
pub fn receive(
    message_buffer: &mut [u8],
) -> ReceivedMessage {
    unsafe {
        let mut rxinfo = MaybeUninit::uninit();
        _sys_receive(
            message_buffer.as_mut_ptr(),
            message_buffer.len(),
            rxinfo.as_mut_ptr(),
        );
        rxinfo.assume_init()
    }
}

/// Information about a message from `receive`.
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

/// Replies to a received message.
///
/// `task` designates which caller to reply to. This should be the value of
/// `received_message.sender` from `receive`.
///
/// `message` is the data to send in the reply.
///
/// # Reply does not return errors
///
/// `reply` does not return a `Result`, i.e. it cannot fail recoverably. This is
/// to prevent higher-importance server tasks from needing to deal with nuisance
/// behavior from clients. For example, once a server has completed a work unit,
/// it very likely doesn't care if its client dies just as the reply is being
/// sent -- it certainly won't be informed if the client dies *just after.*
pub fn reply(
    task: TaskName,
    code: u32,
    message: &[u8],
) {
    unsafe {
        _sys_reply(task, code, message.as_ptr(), message.len())
    }
}

/// Sets the current task's notification mask. 0 bits are disabled (masked), 1
/// bits are enabled (unmasked).
pub fn set_notification_mask(mask: u32) {
    unsafe {
        _sys_notmask(0, mask)
    }
}

/// Unmasks any notifications corresponding to 1 bits in the parameter. This
/// just ORs the parameter into the notification mask.
pub fn unmask_notifications(mask: u32) {
    unsafe {
        _sys_notmask(!0, mask)
    }
}

/// Masks any notifications corresponding to 1 bits in the parameter. This ANDs
/// the complement into the notification mask.
pub fn mask_notifications(mask: u32) {
    unsafe {
        _sys_notmask(!mask, 0)
    }
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
    let (code, _len) = send_untyped(
        TaskName(0),
        KernelOps::EnableInterrupts as u16,
        &[mask as u8, (mask >> 8) as u8, (mask >> 16) as u8, (mask >> 24) as u8],
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
