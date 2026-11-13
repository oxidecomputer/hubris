// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

use ringbuf::{counted_ringbuf, ringbuf_entry};
use temperance::{
    Served, ServerError, ServerIoError,
    io::{
        Storage,
        server::{Backend, Io, RawIoFrame},
    },
};
use userlib::{
    ReplyFaultReason, TaskId, sys_recv_open, sys_reply, sys_reply_fault,
};

use crate::{REPLY_OK, REPLY_SERVER_FAILED, decode_operation};

#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    /// TODO: Add docs
    Processed,
    /// TODO: Add docs
    Rejected(#[count(children)] Rejection),
    /// TODO: Add docs
    Dropped(#[count(children)] Rejection),
    /// TODO: Add docs
    Ipc(#[count(children)] IpcServerError),
    /// TODO: Add docs
    Faulted(#[count(children)] Fault),
    /// TODO: Add docs
    ServerFailed,
    /// TODO: Add docs
    UnknownIoError,
}

counted_ringbuf!(Trace, 16, Trace::None);

#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
enum Fault {
    UndefinedOperation,
    BadMessageSize,
}

/// TODO: Add docs
#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
pub enum IpcServerError {
    /// TODO: Add docs
    ResponseTooLarge,
}

// TODO(robo): Copied from temperance-net; share it?
/// TODO: Add docs
#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
enum Rejection {
    RequestHeaderDeserialize,
    RequestWrongOperation,
    RequestVersionMismatch,
    UnknownMethod,
    RequestBodyDeserialize,
    RequestBodyTrailingBytes,
    ResponseHeaderSerialize,
    ResponseSerialize,
    // TODO: Add comment
    Unknown,
}

impl From<&ServerError> for Rejection {
    fn from(e: &ServerError) -> Self {
        match e {
            ServerError::RequestHeaderDeserialize(_) => {
                Self::RequestHeaderDeserialize
            }
            ServerError::RequestWrongOperation => Self::RequestWrongOperation,
            ServerError::RequestVersionMismatch => Self::RequestVersionMismatch,
            ServerError::UnknownMethod => Self::UnknownMethod,
            ServerError::RequestBodyDeserialize(_) => {
                Self::RequestBodyDeserialize
            }
            ServerError::RequestBodyTrailingBytes => {
                Self::RequestBodyTrailingBytes
            }
            ServerError::ResponseHeaderSerialize(_) => {
                Self::ResponseHeaderSerialize
            }
            ServerError::ResponseSerialize(_) => Self::ResponseSerialize,
            _ => Self::Unknown,
        }
    }
}

/// TODO: Add docs
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct IpcMeta {
    /// TODO: Add docs
    pub sender: TaskId,
    /// TODO: Add docs
    pub flags: u16,
    /// TODO: Add docs
    pub response_capacity: usize,
}

/// TODO: Add docs
pub struct IpcServer {
    notification_mask: u32,
    notifications: u32,
    pending: Option<TaskId>,
}

impl IpcServer {
    /// TODO: Add docs
    pub fn new(notification_mask: u32) -> Self {
        Self {
            notification_mask,
            notifications: 0,
            pending: None,
        }
    }

    fn fault(&mut self, sender: TaskId, reason: Fault) {
        ringbuf_entry!(Trace::Faulted(reason));
        let reason = match reason {
            Fault::UndefinedOperation => ReplyFaultReason::UndefinedOperation,
            Fault::BadMessageSize => ReplyFaultReason::BadMessageSize,
        };
        sys_reply_fault(sender, reason);
    }
}

impl Io for IpcServer {
    type Meta = IpcMeta;
    type Error = IpcServerError;

    fn recv_one_frame_raw<'data>(
        &mut self,
        incoming: &'data mut [u8],
    ) -> Result<Option<RawIoFrame<'data, Self::Meta>>, ServerIoError<Self::Error>>
    {
        // TODO: Discuss why the slice is taken after the loop.
        let (meta, len) = loop {
            let msg = sys_recv_open(incoming, self.notification_mask);
            if msg.sender == TaskId::KERNEL {
                self.notifications |= msg.operation;
                return Ok(None);
            }
            let Some(flags) = decode_operation(msg.operation) else {
                self.fault(msg.sender, Fault::UndefinedOperation);
                continue;
            };
            if msg.message_len > incoming.len() {
                self.fault(msg.sender, Fault::BadMessageSize);
                continue;
            }
            self.pending = Some(msg.sender);
            let meta = IpcMeta {
                sender: msg.sender,
                flags,
                response_capacity: msg.response_capacity,
            };
            break (meta, msg.message_len);
        };
        Ok(Some(RawIoFrame {
            meta,
            raw: &incoming[..len],
        }))
    }

    fn send_one_frame_raw(
        &mut self,
        outgoing: RawIoFrame<'_, Self::Meta>,
    ) -> Result<(), ServerIoError<Self::Error>> {
        let RawIoFrame { meta, raw } = outgoing;
        self.pending = None;
        if raw.len() > meta.response_capacity {
            self.fault(meta.sender, Fault::BadMessageSize);
            return Err(ServerIoError::Io(IpcServerError::ResponseTooLarge));
        }
        sys_reply(meta.sender, REPLY_OK, raw);
        Ok(())
    }
}

/// TODO: Add docs
pub struct IpcBackend<S: 'static> {
    io: IpcServer,
    storage: &'static mut S,
}

impl<S: Storage> IpcBackend<S> {
    /// TODO: Add docs
    pub fn new(io: IpcServer, storage: &'static mut S) -> Self {
        Self { io, storage }
    }

    /// TODO: Add docs
    pub fn serve_forever<T, F, N>(
        &mut self,
        server: &mut T,
        mut serve_one: F,
        mut on_notification: N,
    ) -> !
    where
        F: FnMut(
            &mut T,
            &mut Self,
        ) -> Result<Served, ServerIoError<IpcServerError>>,
        N: FnMut(&mut T, u32),
    {
        loop {
            match serve_one(server, self) {
                Ok(Served::Idle) => {}
                Ok(Served::Responded) => {
                    ringbuf_entry!(Trace::Processed);
                }
                Ok(Served::Rejected(e)) => {
                    ringbuf_entry!(Trace::Rejected((&e).into()));
                }
                Err(ServerIoError::Server(e)) => {
                    ringbuf_entry!(Trace::Dropped((&e).into()));
                }
                Err(ServerIoError::Io(e)) => {
                    ringbuf_entry!(Trace::Ipc(e));
                }
                // TODO: Add comment
                Ok(_) => {}
                Err(_) => {
                    ringbuf_entry!(Trace::UnknownIoError);
                }
            }

            // TODO: Discuss the invariant that every received message gets a
            // reply.
            if let Some(sender) = self.io.pending.take() {
                ringbuf_entry!(Trace::ServerFailed);
                sys_reply(sender, REPLY_SERVER_FAILED, &[]);
            }

            let bits = core::mem::take(&mut self.io.notifications);
            if bits != 0 {
                on_notification(server, bits);
            }
        }
    }
}

impl<S: Storage> Backend for IpcBackend<S> {
    type Storage = S;
    type Io = IpcServer;

    #[inline]
    fn parts(&mut self) -> (&mut Self::Storage, &mut Self::Io) {
        let Self { io, storage } = self;
        (storage, io)
    }
}
