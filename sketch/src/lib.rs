#![no_std]

use core::marker::PhantomData;
use bitflags::bitflags;

// Our assembly language entry points
extern "C" {
    fn _sys_send(descriptor: &mut SendDescriptor<'_>) -> SendResponse;
}

/// A type for designating a task you want to interact with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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

#[repr(C)]
struct SendResponse {
    success: bool,
    param: usize,
}

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
    #[repr(transparent)]
    struct LeaseAttributes: u32 {
        const READ = 1 << 0;
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
