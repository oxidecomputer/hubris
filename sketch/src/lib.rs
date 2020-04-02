#![no_std]

use core::marker::PhantomData;
use core::mem::MaybeUninit;

use bitflags::bitflags;

// Our assembly language entry points
extern "C" {
    fn _sys_send(descriptor: &mut SendDescriptor<'_>) -> SendResponse;
    fn _sys_receive(buffer: *mut u8, buffer_len: usize, rxinfo: *mut ReceivedMessage);
    fn _sys_reply(task: TaskName, message: *const u8, len: usize);
    fn _sys_notmask(and: u32, or: u32);
}

/// A type for designating a task you want to interact with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct TaskName(pub u16);

/// Sends a message and waits for a reply.
/// 
/// The target task is named by `dest`. If `dest` is a name that is stale (i.e.
/// the target has reset since we last interacted), this returns
/// `DeathComesForUsAll`.
///
/// The request to transmit is identified by `request`. The contents of the
/// slice will be transferred by the kernel into a place defined by the
/// recipient if/when the message is delivered. If the recipient hasn't given
/// enough room for `request` in its entirety, you will not be informed of this,
/// but the recipient will.
///
/// `response` gives the buffer in which the response message, if any, should be
/// written. The message will be written if (1) this message is received and (2)
/// the recipient replies to us. If the message fits, its size is returned.
/// Otherwise, the first `response.len()` bytes are written and
/// `OverlyEnthusiasticResponse` is returned.
///
/// The `leases` table optionally makes sections of your address space visible
/// to the peer without additional copies. Leases are revoked before this
/// returns, so it's equivalent to borrowing.
pub fn send_untyped(
    dest: TaskName,
    request: &[u8],
    response: &mut [u8],
    leases: &[Lease<'_>],
) -> Result<usize, SendError> {
    let r = unsafe {
        _sys_send(&mut SendDescriptor {
            dest: dest.0,
            request_base: request.as_ptr(),
            request_len: request.len(),
            response_base: response.as_mut_ptr(),
            response_len: response.len(),
            lease_base: leases.as_ptr(),
            lease_len: leases.len(),
        })
    };
    if r.success {
        Ok(r.param)
    } else {
        Err(match r.param {
            0 => SendError::DeathComesForUsAll,
            1 => SendError::OverlyEnthusiasticResponse,
            _ => panic!(),
        })
    }
}

/// Internal record generated on the stack to describe our desired action to the
/// kernel.
///
/// We could totally pass all this stuff in registers, which would not only be
/// faster, but it would keep the kernel from needing to use carefully checked
/// memory access to inspect our userland stack on the fast IPC path. However,
/// doing this without inline assembler is kind of a pain, so I'm doing it the
/// awkward-but-stable way for now.
///
/// The contents of this struct matches the args to `send_untyped` except slices
/// are flattened into pairs of words.
#[repr(C)]
struct SendDescriptor<'a> {
    dest: u16,
    request_base: *const u8,
    request_len: usize,
    response_base: *mut u8,
    response_len: usize,
    lease_base: *const Lease<'a>,
    lease_len: usize,
}

/// Internal defined-layout version of a send response.
///
/// This exists because the memory/register layout of `Result<usize, SendError>`
/// is not promised to be stable.
#[repr(C)]
struct SendResponse {
    /// If `true`, `param` is the number of bytes in the response. If `false`,
    /// `param` enumerates the error condition.
    success: bool,
    /// See above.
    param: usize,
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

/// Things that can go wrong when sending, under *normal operation.*
///
/// Conditions that are conspicuously missing from this set:
///
/// - Can't send to that task because of MAC: I would rather treat any
///   MAC violation as a fault that gets escalated to supervision.
///
/// - Message larger than supported by kernel: message size limits are known at
///   compile time, and most messages are expected to be statically sized. An
///   attempt to send a message that's too big is a malfunction and should also
///   be treated as a fault.
///
/// - Attempt to send from, or receive into, sections of the address space that
///   you do not own: malfunction, fault.
///
/// Perhaps you are noticing a trend.
#[derive(Copy, Clone, Debug)]
pub enum SendError {
    /// The peer restarted since you last spoke to it. You might need to redo
    /// some work.
    DeathComesForUsAll,
    /// Your message was accepted and processed, but the peer returned a
    /// response that was larger than the buffer you offered. The prefix of the
    /// response has been deposited for your inspection.
    OverlyEnthusiasticResponse,
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
    message: &[u8],
) {
    unsafe {
        _sys_reply(task, message.as_ptr(), message.len())
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
    send_untyped(
        TaskName(0),
        &[1, 0, 0, 0, mask as u8, (mask >> 8) as u8, (mask >> 16) as u8, (mask >> 24) as u8],
        &mut [],
        &[],
    ).unwrap();
}

