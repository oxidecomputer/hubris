// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

use ringbuf::{counted_ringbuf, ringbuf_entry};
use temperance::{
    Served, ServerError, ServerIoError,
    io::{
        Storage, StorageView,
        server::{Backend, Io, RawIoFrame},
    },
};
use userlib::{
    BorrowInfo, DEFECT, LeaseAttributes, RecvMessage, ReplyFaultReason, TaskId,
    sys_borrow_info, sys_borrow_read, sys_borrow_write, sys_recv_open,
    sys_reply, sys_reply_fault,
};

use crate::{
    FLAG_RESP_LEASED, FLAG_RQST_LEASED, LEASE_THRESHOLD, LEASES, REPLY_OK,
    REPLY_SERVER_FAILED, RESP_LEN_SIZE, decode_operation,
};

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
    BadLeases,
    ReplyBufferTooSmall,
}

/// TODO: Add docs
#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
pub enum IpcServerError {
    /// TODO: Add docs
    ResponseTooLarge,
    /// TODO: Add docs
    ClientDied,
    /// TODO: Add docs
    LeaseWriteFailed,
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
    /// TODO: Add docs
    pub response_lease: Option<usize>,
}

/// TODO: Add docs
pub struct IpcServer {
    notification_mask: u32,
    notifications: u32,
    pending: Option<TaskId>,
}

// TODO: Add comment
enum Reject {
    Fault(Fault),
    ClientDied,
}

impl From<Fault> for Reject {
    fn from(f: Fault) -> Self {
        Self::Fault(f)
    }
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
            Fault::BadLeases => ReplyFaultReason::BadLeases,
            Fault::ReplyBufferTooSmall => ReplyFaultReason::ReplyBufferTooSmall,
        };
        sys_reply_fault(sender, reason);
    }

    // TODO: Discuss lease index assignment.
    fn accept(
        &mut self,
        msg: &RecvMessage,
        incoming: &mut [u8],
    ) -> Result<(IpcMeta, usize), Reject> {
        let flags =
            decode_operation(msg.operation).ok_or(Fault::UndefinedOperation)?;
        let rqst_leased = LEASES && flags & FLAG_RQST_LEASED != 0;
        let resp_leased = LEASES && flags & FLAG_RESP_LEASED != 0;

        let lease_count = usize::from(rqst_leased) + usize::from(resp_leased);
        if msg.lease_count != lease_count {
            return Err(Fault::BadLeases.into());
        }

        let len = if rqst_leased {
            if msg.message_len != 0 {
                return Err(Fault::BadMessageSize.into());
            }
            let info = borrow_info(msg.sender, 0, LeaseAttributes::READ)?;
            let dest =
                incoming.get_mut(..info.len).ok_or(Fault::BadMessageSize)?;
            let (rc, n) = sys_borrow_read(msg.sender, 0, 0, dest);
            check_borrow(rc, n, info.len)?;
            info.len
        } else {
            if msg.message_len > incoming.len() {
                return Err(Fault::BadMessageSize.into());
            }
            msg.message_len
        };

        let (response_capacity, response_lease) = if resp_leased {
            let idx = usize::from(rqst_leased);
            let info = borrow_info(msg.sender, idx, LeaseAttributes::WRITE)?;
            if msg.response_capacity < RESP_LEN_SIZE {
                return Err(Fault::ReplyBufferTooSmall.into());
            }
            (info.len, Some(idx))
        } else {
            (msg.response_capacity, None)
        };

        let meta = IpcMeta {
            sender: msg.sender,
            flags,
            response_capacity,
            response_lease,
        };
        Ok((meta, len))
    }
}

fn borrow_info(
    sender: TaskId,
    idx: usize,
    needs: LeaseAttributes,
) -> Result<BorrowInfo, Reject> {
    // TODO: Discuss why a failed BORROW_INFO means the client is gone.
    let info = sys_borrow_info(sender, idx).ok_or(Reject::ClientDied)?;
    if !info.attributes.contains(needs) {
        return Err(Fault::BadLeases.into());
    }
    Ok(info)
}

fn check_borrow(rc: u32, n: usize, expected: usize) -> Result<(), Reject> {
    // TODO: Discuss why DEFECT means the client is gone here.
    if rc == DEFECT {
        return Err(Reject::ClientDied);
    }
    if rc != 0 || n != expected {
        return Err(Fault::BadLeases.into());
    }
    Ok(())
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
            match self.accept(&msg, incoming) {
                Ok(accepted) => {
                    self.pending = Some(msg.sender);
                    break accepted;
                }
                Err(Reject::Fault(f)) => self.fault(msg.sender, f),
                Err(Reject::ClientDied) => {
                    ringbuf_entry!(Trace::Ipc(IpcServerError::ClientDied));
                }
            }
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
            self.fault(meta.sender, Fault::ReplyBufferTooSmall);
            return Err(ServerIoError::Io(IpcServerError::ResponseTooLarge));
        }
        // TODO: Discuss why `LEASES` is checked here too.
        match meta.response_lease {
            Some(idx) if LEASES => {
                let (rc, n) = sys_borrow_write(meta.sender, idx, 0, raw);
                match check_borrow(rc, n, raw.len()) {
                    Ok(()) => {}
                    Err(Reject::ClientDied) => {
                        return Err(ServerIoError::Io(
                            IpcServerError::ClientDied,
                        ));
                    }
                    Err(Reject::Fault(f)) => {
                        self.fault(meta.sender, f);
                        return Err(ServerIoError::Io(
                            IpcServerError::LeaseWriteFailed,
                        ));
                    }
                }
                let len = raw.len() as u32;
                sys_reply(meta.sender, REPLY_OK, &len.to_le_bytes());
            }
            _ => sys_reply(meta.sender, REPLY_OK, raw),
        }
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
        // TODO: Discuss why large servers need the `leases` feature.
        if !LEASES {
            let StorageView { rqst_buf, resp_buf } = storage.buffers();
            assert!(
                rqst_buf.len() <= LEASE_THRESHOLD
                    && resp_buf.len() <= LEASE_THRESHOLD,
                "temperance-ipc: buffers over LEASE_THRESHOLD need `leases`",
            );
        }
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
