// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! High-level user interface.
//!
//! This is intended to provide a more ergonomic interface than the raw
//! syscalls.

use abi::TaskId;
use core::cell::Cell;
use core::marker::PhantomData;
use zerocopy::{AsBytes, FromBytes, LayoutVerified};

use crate::{
    sys_borrow_info, sys_borrow_read, sys_borrow_write, sys_get_timer,
    sys_recv, sys_recv_closed, sys_recv_open, sys_reply, sys_send,
    sys_set_timer, BorrowInfo, ClosedRecvError, FromPrimitive,
};

const INTERNAL_TIMER_NOTIFICATION: u32 = 1 << 31;

/// Receives a message, or a notification, and handles it.
///
/// This is a wrapper for the `sys_recv` syscall that takes care of paperwork on
/// your behalf.
///
/// `buffer` should be large enough to contain the largest valid message that
/// can be sent to your task.
///
/// `mask` is a bitmask describing the set of notifications to accept. Bits set
/// (1) in this mask indicate that a notification is allowed.
///
/// `state` is a value of your choice that will get passed to whichever closure
/// -- `notify` or `msg` -- gets executed. More on this below.
///
/// `notify` will be called if the kernel provides a notification instead of a
/// message. Its only parameter: a `u32` with a bit set for each pending
/// notification.
///
/// `msg` will be called if the kernel provides a proper message from another
/// task. It will be passed: the `state`, the decoded operation, and a `Message`
/// describing the contents.
///
/// # About operation decoding
///
/// `hl::recv` operates on a type, `O`, that you choose. This represents the
/// operation code, and must implement `FromPrimitive` so we can try to make an
/// `O` from a `u32`.
///
/// Whenever a message (not a notification) arrives, we'll attempt to make an
/// `O` out of its operation code using `FromPrimitive::from_u32`. If this
/// succeeds, we pass the result into your `msg` closure.
///
/// If it *fails*, we immediately respond to the caller with an error code (1)
/// that has conventionally come to mean "bad operation."
///
/// # About error encoding
///
/// Your `msg` closure can return an error type, `E`. If this occurs, `recv`
/// will convert it into a `u32` and send it back to the caller as the response
/// code to the IPC.
///
/// Because the response code 0 means "success," your error type `E` should not
/// have a value corresponding to 0, or things will get weird for you.
///
/// # About the `state` parameter
///
/// If `recv` took only one closure, it could borrow exclusively (`&mut`)
/// anything it wanted from the caller's stack frame, and your life would be
/// simple and easy. (For one way of achieving a simple and easy life, see
/// `recv_without_notification`.)
///
/// However, `recv` takes *two* closures. If both of these closures need access
/// to the same mutable state in your server's stack frame -- and they almost
/// certainly do! -- the compiler will complain when they both try to borrow it.
///
/// To fix this, don't borrow it in the closures -- borrow it and pass it as
/// `state`. It will be provided to whichever closure is executed, as its first
/// argument.
///
/// If you don't need this, just pass `()`.
pub fn recv<'a, O, E, S>(
    buffer: &'a mut [u8],
    mask: u32,
    state: S,
    notify: impl FnOnce(S, u32),
    msg: impl FnOnce(S, O, Message<'a>) -> Result<(), E>,
) where
    O: FromPrimitive,
    E: Into<u32>,
{
    let rm = sys_recv_open(buffer, mask);
    let sender = rm.sender;
    if rm.sender == TaskId::KERNEL {
        notify(state, rm.operation);
    } else {
        if let Some(op) = O::from_u32(rm.operation) {
            let m = Message {
                buffer: &buffer[..rm.message_len],
                sender: rm.sender,
                response_capacity: rm.response_capacity,
                lease_count: rm.lease_count,
            };
            if let Err(e) = msg(state, op, m) {
                sys_reply(sender, e.into(), &[]);
            }
        } else {
            sys_reply(sender, 1, &[]);
        }
    }
}

/// Variant of `recv` that doesn't allow notifications.
///
/// This is exactly the same as passing a notification mask of 0 and a
/// do-nothing notification handler.
///
/// Note that `recv`'s `state` parameter isn't present -- because you're only
/// passing one closure in, you can borrow whatever you'd like.
pub fn recv_without_notification<'a, O, E>(
    buffer: &'a mut [u8],
    msg: impl FnOnce(O, Message<'a>) -> Result<(), E>,
) where
    O: FromPrimitive,
    E: Into<u32>,
{
    recv(buffer, 0, (), |_, _| (), |_, op, m| msg(op, m))
}

/// Variant of `recv` that can be configured at runtime to receive from a
/// specific task only (closed receive) by setting `source` to `Some(task_id)`,
/// or to receive from all callers (`source` of `None`).
///
/// See `recv` for more description.
pub fn recv_from<'a, O, E, S>(
    source: Option<TaskId>,
    buffer: &'a mut [u8],
    mask: u32,
    state: S,
    notify: impl FnOnce(S, u32),
    msg: impl FnOnce(S, O, Message<'a>) -> Result<(), E>,
) -> Result<(), ClosedRecvError>
where
    O: FromPrimitive,
    E: Into<u32>,
{
    let rm =
        sys_recv(buffer, mask, source).map_err(|_| ClosedRecvError::Dead)?;
    let sender = rm.sender;
    if rm.sender == TaskId::KERNEL {
        notify(state, rm.operation);
        Ok(())
    } else {
        if let Some(op) = O::from_u32(rm.operation) {
            let m = Message {
                buffer: &buffer[..rm.message_len],
                sender: rm.sender,
                response_capacity: rm.response_capacity,
                lease_count: rm.lease_count,
            };
            if let Err(e) = msg(state, op, m) {
                sys_reply(sender, e.into(), &[]);
            }
        } else {
            sys_reply(sender, 1, &[]);
        }
        Ok(())
    }
}

/// Variant of `recv_without_notification` that can be configured at runtime to
/// receive from a specific task only (closed receive) by setting `source` to
/// `Some(task_id)`, or to receive from all callers (`source` of `None`).
///
/// See `recv_without_notification` for more description.
pub fn recv_from_without_notification<'a, O, E>(
    source: Option<TaskId>,
    buffer: &'a mut [u8],
    msg: impl FnOnce(O, Message<'a>) -> Result<(), E>,
) -> Result<(), ClosedRecvError>
where
    O: FromPrimitive,
    E: Into<u32>,
{
    recv_from(source, buffer, 0, (), |_, _| (), |_, op, m| msg(op, m))
}

/// Represents a received message (not a notification).
///
/// This type gets passed by `recv` (and related operations) into the message
/// handler.
///
/// If you know the operation code, you can work out what type of message is
/// expected for that operation. At this point the first (and only) thing you
/// probably want to do with a `Message` is call `fixed` or `fixed_with_leases`.
pub struct Message<'a> {
    buffer: &'a [u8],
    response_capacity: usize,
    lease_count: usize,
    sender: TaskId,
}

impl<'a> Message<'a> {
    /// Parses this message as a fixed-size value of type `M`, and prepares to
    /// (maybe, eventually) send a response of type `R`.
    ///
    /// If the caller sent a message whose size doesn't match `M` (too big *or*
    /// too small), or prepared a response buffer too small for `R`, this
    /// returns `None`. (We return `Option` instead of `Result` to allow you to
    /// use your server's own error type using `ok_or`.)
    ///
    /// If the message is the right size for an `M` and there's enough room for
    /// us to return an `R`, returns `Some((msg, caller))`, where `msg` is a
    /// reference into the original buffer reinterpreted as an `M`, and `caller`
    /// is a typed handle to reply to the caller, eventually, maybe.
    ///
    /// # Panics
    ///
    /// If the buffer you originally passed to `hl::recv` is not correctly
    /// aligned for type `M`. The easiest way to ensure this is to use an
    /// [`Unaligned`][zerocopy::Unaligned] type.
    pub fn fixed<M, R>(self) -> Option<(&'a M, Caller<R>)>
    where
        M: FromBytes,
        R: AsBytes,
    {
        let caller = Caller::from(self.sender);
        if self.buffer.len() != core::mem::size_of::<M>()
            || self.response_capacity < core::mem::size_of::<R>()
        {
            None
        } else {
            let msg = LayoutVerified::<_, M>::new(self.buffer)
                .expect("buffer has wrong alignment")
                .into_ref();
            Some((msg, caller))
        }
    }

    /// Variant of `fixed` that, in addition to doing everything `fixed` does,
    /// *also* verifies that the caller sent exactly `n` leases.
    ///
    /// # Panics
    ///
    /// This will panic under the same circumstances as `fixed`.
    pub fn fixed_with_leases<M, R>(self, n: usize) -> Option<(&'a M, Caller<R>)>
    where
        M: FromBytes,
        R: AsBytes,
    {
        if self.lease_count != n {
            None
        } else {
            self.fixed()
        }
    }

    pub fn lease_count(&self) -> usize {
        self.lease_count
    }
}

/// A typed handle to a task, used to send a single reply of type `R`.
pub struct Caller<R> {
    id: TaskId,
    _phantom: PhantomData<fn(R)>,
}

/// This impl is available if you want to synthesize a `Caller` for some unusual
/// reason, but in general, you should get your `Caller` from operations like
/// `Message::fixed`.
impl<R> From<TaskId> for Caller<R> {
    fn from(id: TaskId) -> Self {
        Caller {
            id,
            _phantom: PhantomData,
        }
    }
}

impl<R> Caller<R> {
    /// Sends a successful reply message of type `R`, consuming the handle.
    pub fn reply(self, message: R)
    where
        R: AsBytes,
    {
        sys_reply(self.id, 0, message.as_bytes())
    }

    /// Sends a failure message with response code `rc`, consuming the handle.
    ///
    /// Because a response code of 0 conventionally means "success," `rc` should
    /// not convert to 0, or things will get weird for you.
    pub fn reply_fail(self, rc: impl Into<u32>) {
        sys_reply(self.id, rc.into(), &[]);
    }

    /// Derives a borrow handle to borrow number `index`.
    ///
    /// See the caveats on `Borrow` about what holding a borrow handle does, and
    /// does not, mean.
    pub fn borrow(&self, index: usize) -> Borrow<'_> {
        Borrow {
            id: self.id,
            index,
            _phantom: PhantomData,
        }
    }

    /// Extracts the `TaskId` of a caller.
    pub fn task_id(&self) -> TaskId {
        self.id
    }
}

/// A handle representing a particular numbered borrow from a particular caller.
///
/// Having a borrow handle means basically nothing -- in particular, it does not
/// mean that the caller has a valid corresponding borrow. It's just a
/// convenient way to talk about and act on a borrow.
///
/// The borrow handle borrows the `Caller` to keep you from accidentally holding
/// the borrow after you reply to the caller (causing it to revoke the lease).
/// This is an error-robustness thing and not a safety thing.
#[derive(Clone)]
pub struct Borrow<'caller> {
    id: TaskId,
    index: usize,
    _phantom: PhantomData<&'caller ()>,
}

impl Borrow<'_> {
    /// Gets information on this borrow from the kernel.
    ///
    /// This is a wrapper for the `sys_borrow_info` syscall.
    ///
    /// If the borrow doesn't exist -- either because it never did, or because
    /// the caller has been killed asynchronously -- this returns `None`.
    pub fn info(&self) -> Option<BorrowInfo> {
        sys_borrow_info(self.id, self.index)
    }

    /// Starting at offset `offset` within the borrow, reads exactly
    /// `dest.len()` bytes into `dest`.
    ///
    /// This can fail because the client has defected or was killed, the borrow
    /// doesn't exist, the borrow doesn't allow reading, or you're trying to
    /// read off the end. All these conditions return `None` because, in
    /// general, we don't expect servers to do anything except reject the
    /// client.
    pub fn read_fully_at(&self, offset: usize, dest: &mut [u8]) -> Option<()> {
        let (rc, n) = sys_borrow_read(self.id, self.index, offset, dest);
        if rc != 0 {
            None
        } else if n != dest.len() {
            None
        } else {
            Some(())
        }
    }

    /// Starting at offset `offset` within the borrow, reads one item of type
    /// `T` and returns it.
    ///
    /// This can fail because the client has defected or was killed, the borrow
    /// doesn't exist, the borrow doesn't allow reading, or you're trying to
    /// read off the end. All these conditions return `None` because, in
    /// general, we don't expect servers to do anything except reject the
    /// client.
    ///
    /// Even if `T` requires alignment greater than 1 byte, no alignment
    /// requirements is placed on the *client* side.
    pub fn read_at<T>(&self, offset: usize) -> Option<T>
    where
        T: Default + FromBytes + AsBytes,
    {
        // NOTE: the default requirement could be lifted if we do some unsafe
        // uninitialized buffer shenanigans.
        let mut dest = T::default();
        let (rc, n) =
            sys_borrow_read(self.id, self.index, offset, dest.as_bytes_mut());
        if rc != 0 {
            None
        } else if n != core::mem::size_of::<T>() {
            None
        } else {
            Some(dest)
        }
    }

    /// Starting at offset `offset` within the borrow, writes one item of type
    /// `T`.
    ///
    /// This can fail because the client has defected or was killed, the borrow
    /// doesn't exist, the borrow doesn't allow writing, or you're trying to
    /// write past the end. All these conditions return `None` because, in
    /// general, we don't expect servers to do anything except reject the
    /// client.
    ///
    /// Even if `T` requires alignment greater than 1 byte, no alignment
    /// requirements is placed on the *client* side.
    pub fn write_at<T>(&self, offset: usize, value: T) -> Option<()>
    where
        T: AsBytes,
    {
        let (rc, n) =
            sys_borrow_write(self.id, self.index, offset, value.as_bytes());
        if rc != 0 {
            None
        } else if n != core::mem::size_of::<T>() {
            None
        } else {
            Some(())
        }
    }

    pub fn write_fully_at(&self, offset: usize, src: &[u8]) -> Option<()> {
        let (rc, n) = sys_borrow_write(self.id, self.index, offset, src);
        if rc != 0 {
            None
        } else if n != src.len() {
            None
        } else {
            Some(())
        }
    }
}

/// Trait implemented by types that represent a message sent to another task.
///
/// A `Call` type `C` has four parts: the contents of a value of type `C`, which
/// form the actual message, and three associated items:
///
/// - The expected type of the response, `C::Response`, which is how returned
///   bytes will be interpreted.
///
/// - The error type, `C::Err`, that will be constructed from any non-zero
///   response code.
///
/// - The operation number, `C::OP`, used to identify this operation to its
///   recipient and inform their parsing.
///
/// Types implementing `Call` can be used with `hl::send` to get simple,
/// type-safe messaging.
///
/// # Limitations
///
/// - The `Response` type can't be a complex enum, because it must be valid for
///   any sequence of bytes of the appropriate size. Which is to say, `hl::send`
///   won't do any validation of response *contents.*
///
/// - While it's possible to implement `Call` for an `enum`, the lack of a
///   well-defined ABI for complex enums means the server code for deserializing
///   the message will be complex. You might choose to go with multiple
///   operations using structs instead. (This is not specific to the `Call`
///   trait.)
///
/// You can always call `sys_send` directly to circumvent these, if desired.
pub trait Call: AsBytes {
    /// Type of the expected response on success (response code of 0).
    type Response: FromBytes;
    /// Type of the error returned on failure (response code not 0).
    type Err: From<u32>;
    /// Operation code to send to the server.
    const OP: u16;
}

/// Typed version of `sys_send` that sends a value to another task and collects
/// a response.
///
/// `send` will:
///
/// - Reinterpret `message` as a slice of bytes,
/// - Allocate a buffer large enough to receive a response of type
///   `M::Response`,
/// - Send `message` to `target`,
/// - Block waiting for a response,
/// - Inspect the response code: if zero, the response is reinterpreted as
///   `M::Response` and returned in `Ok`. Non-zero response codes are passed to
///   `M::Err`'s impl of `From<u32>` for conversion and returned in `Err`.
///
/// This does *not* require either `M` or `M::Response` to be `Unaligned` -- it
/// will correctly manage alignment on our side.
///
/// This will work for any type `M` that implements `Call`, though note that the
/// client and server must *agree* on the types: no type information is sent.
///
/// # Panics
///
/// If the server sends back a successful response that is the wrong size for
/// `M::Response`. This indicates a serious bug, so it's not something we would
/// make every client handle every time by returning an `Err`.
pub fn send<M>(target: TaskId, message: &M) -> Result<M::Response, M::Err>
where
    M: Call,
{
    use core::mem::MaybeUninit;

    // Engage in some unsafe shenanigans to obtain an uninitialized buffer with
    // the right size and alignment to contain one M::Response. Recall that
    // M::Response is FromBytes (as required by Call).
    let mut response: MaybeUninit<M::Response> = MaybeUninit::uninit();
    let rslice = unsafe {
        core::slice::from_raw_parts_mut(
            response.as_mut_ptr() as *mut u8,
            core::mem::size_of_val(&response),
        )
    };

    let (code, rlen) = sys_send(target, M::OP, message.as_bytes(), rslice, &[]);

    if code == 0 {
        if rlen == core::mem::size_of_val(&response) {
            Ok(unsafe { response.assume_init() })
        } else {
            // The trust relationship from client to server requires that
            // servers behave, e.g. reply to messages instead of merely dropping
            // them. For now, we'll extend this relationship to say that a
            // client can panic if a server sends back an ill response.
            panic!();
        }
    } else {
        Err(M::Err::from(code))
    }
}

/// Typed version of `sys_send` that sends a value to another task and collects
/// a response, retrying automatically if that task has restarted. This is a
/// variant on `send` for operations that are idempotent (because the server may
/// have performed your operation and then crashed before replying, or may not
/// have received it before crashing, and both will cause a retry).
///
/// `send_with_retry` will:
///
/// - Reinterpret `message` as a slice of bytes,
/// - Allocate a buffer large enough to receive a response of type
///   `M::Response`,
/// - Send `message` to `target`,
/// - Block waiting for a response,
/// - Inspect the response code:
///   - If zero, the response is reinterpreted as `M::Response` and returned in
///     `Ok`.
///   - If the code is in the "dead" range, indicating a peer failure, the
///     generation number is extracted from the response and `target` is
///     updated.  The IPC is then retried.
///   - Any other non-zero response code is passed to `M::Err`'s impl of
///     `From<u32>` for conversion and returned in `Err`.
///
/// This does *not* require either `M` or `M::Response` to be `Unaligned` -- it
/// will correctly manage alignment on our side.
///
/// This will work for any type `M` that implements `Call`, though note that the
/// client and server must *agree* on the types: no type information is sent.
///
/// # Panics
///
/// If the server sends back a successful response that is the wrong size for
/// `M::Response`. This indicates a serious bug, so it's not something we would
/// make every client handle every time by returning an `Err`.
pub fn send_with_retry<M>(
    target: &Cell<TaskId>,
    message: &M,
) -> Result<M::Response, M::Err>
where
    M: Call,
{
    use core::mem::MaybeUninit;

    // Engage in some unsafe shenanigans to obtain an uninitialized buffer with
    // the right size and alignment to contain one M::Response. Recall that
    // M::Response is FromBytes (as required by Call).
    let mut response: MaybeUninit<M::Response> = MaybeUninit::uninit();
    let rslice = unsafe {
        core::slice::from_raw_parts_mut(
            response.as_mut_ptr() as *mut u8,
            core::mem::size_of_val(&response),
        )
    };

    loop {
        let last_target = target.get();
        let (code, rlen) =
            sys_send(last_target, M::OP, message.as_bytes(), rslice, &[]);

        if code == 0 {
            if rlen == core::mem::size_of_val(&response) {
                break Ok(unsafe { response.assume_init() });
            } else {
                // The trust relationship from client to server requires that
                // servers behave, e.g. reply to messages instead of merely dropping
                // them. For now, we'll extend this relationship to say that a
                // client can panic if a server sends back an ill response.
                panic!();
            }
        } else if let Some(g) = abi::extract_new_generation(code) {
            // Task has rolled over, we will update our records and retry.
            target.set(TaskId::for_index_and_gen(last_target.index(), g));
            continue;
        } else {
            break Err(M::Err::from(code));
        }
    }
}

/// Suspends the calling task until the kernel time is `>= time`.
///
/// TODO: once we figure out how to convert between ticks and seconds here, this
/// should take a real unit instead of a tick count.
pub fn sleep_until(time: u64) {
    sys_set_timer(Some(time), INTERNAL_TIMER_NOTIFICATION);
    loop {
        let _ = sys_recv_closed(
            &mut [],
            INTERNAL_TIMER_NOTIFICATION,
            TaskId::KERNEL,
        );
        // We don't actually need to check the results:
        // - The kernel cannot die.
        // - We only agreed to accept notification messages with our timer bit set.
        // - We must assume that the kernel is correct.

        // We do, however, need to check for the possibility of spurious
        // wakeups, by reading the time back.
        if sys_get_timer().now >= time {
            break;
        }
    }
}

/// Suspends the calling task until the kernel time has increased by `ticks`.
///
/// TODO: once we figure out how to convert between ticks and seconds here, this
/// needs to take `Duration`.
pub fn sleep_for(ticks: u64) {
    // By definition, when we observe the kernel time as being some value T, we
    // are some amount of time into the tick that began at T (the time required
    // for us to make the observation). This means that some of that tick has
    // already elapsed. In the extreme case, if we call `sys_get_timer`
    // immediately before the end of a tick, and add 1 to it, we will not sleep
    // for 1 tick -- we'll sleep for zero.
    //
    // Thus we adjust the duration up by 1. This gets us "at-least" semantics:
    // `sleep_for(x)` will sleep for at least `x` full ticks. Note that the task
    // calling `sleep_for` may get woken arbitrarily later if preempted by
    // higher priority tasks, so at-least is generally the best we can do.
    sleep_until(sys_get_timer().now + ticks + 1)
}
