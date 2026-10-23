// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Serving [`nprpc`] interfaces over a UDP socket owned by the `net` task.
//!
//! This crate provides the [`nprpc::io::server::Io`] and
//! [`nprpc::io::server::Backend`] implementations needed to serve an `nprpc`
//! interface from a Hubris task, along with a [`NetBackend::serve_forever`]
//! loop that handles notifications and records errors in a ringbuf.
//!
//! A minimal server task looks like:
//!
//! ```ignore
//! nprpc::autobuffer!(ApiBufs, my_api::composite);
//!
//! static BUFS: ClaimOnceCell<ApiBufs> = ClaimOnceCell::new(ApiBufs::new());
//! let mut backend = NetBackend::new(
//!     UdpSocket::new(net, SocketName::my_socket, notifications::SOCKET_MASK),
//!     BUFS.claim(),
//! );
//! backend.serve_forever(|b| {
//!     my_api::composite::Server::serve_one(&mut server, b)
//! })
//! ```

#![no_std]

use nprpc::{
    Served, ServerError, ServerIoError,
    io::{
        Storage,
        server::{Backend, Io, RawIoFrame},
    },
};
use ringbuf::{counted_ringbuf, ringbuf_entry};
use task_net_api::{
    LargePayloadBehavior, Net, RecvError, SendError, SocketName, UdpMetadata,
};
use userlib::sys_recv_notification;

#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    /// A request was received but could not be served; the client was sent a
    /// `WireError` response.
    Rejected(#[count(children)] Rejection),
    /// We failed to produce any response at all, nothing was sent.
    Dropped(#[count(children)] Rejection),
    /// An error from the `net` task.
    Net(#[count(children)] NetError),
    /// An error kind added in a newer `nprpc` that we don't know about.
    UnknownIoError,
}

counted_ringbuf!(Trace, 16, Trace::None);

/// Errors reported by the `net` task while exchanging frames.
#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
pub enum NetError {
    Recv(#[count(children)] RecvError),
    Send(#[count(children)] SendError),
}

/// Local version of [`ServerError`] for ringbuf'ing.
#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
pub enum Rejection {
    RequestHeaderDeserialize,
    RequestWrongOperation,
    RequestVersionMismatch,
    UnknownMethod,
    RequestBodyDeserialize,
    RequestBodyTrailingBytes,
    ResponseHeaderSerialize,
    ResponseSerialize,
    // non-exhaustive
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

/// A UDP socket owned by this task, served by the `net` task.
pub struct UdpSocket {
    net: Net,
    socket: SocketName,
    notification_mask: u32,
}

impl UdpSocket {
    /// Creates a new socket handle.
    ///
    /// `notification_mask` must be the notification configured for this
    /// socket in the app.toml; it is used to wait for incoming packets and for
    /// space in the outgoing queue.
    pub fn new(net: Net, socket: SocketName, notification_mask: u32) -> Self {
        Self {
            net,
            socket,
            notification_mask,
        }
    }
}

impl Io for UdpSocket {
    type Meta = UdpMetadata;
    type Error = NetError;

    fn recv_one_frame_raw<'data>(
        &mut self,
        incoming: &'data mut [u8],
    ) -> Result<Option<RawIoFrame<'data, Self::Meta>>, ServerIoError<Self::Error>>
    {
        // Packets too large for `incoming` are discarded by `net`, as they
        // can't be a valid request for this interface anyway.
        match self.net.recv_packet(
            self.socket,
            LargePayloadBehavior::Discard,
            incoming,
        ) {
            Ok(meta) => {
                // `net` should never report more than it wrote, but clamp
                // rather than panic. A truncated frame will fail to decode.
                let len = usize::min(meta.size as usize, incoming.len());
                Ok(Some(RawIoFrame {
                    meta,
                    raw: &incoming[..len],
                }))
            }
            Err(RecvError::QueueEmpty) => Ok(None),
            Err(e) => Err(ServerIoError::Io(NetError::Recv(e))),
        }
    }

    fn send_one_frame_raw(
        &mut self,
        outgoing: RawIoFrame<'_, Self::Meta>,
    ) -> Result<(), ServerIoError<Self::Error>> {
        let RawIoFrame { mut meta, raw } = outgoing;
        // overwrite the request len with the response len to reuse the metadata
        meta.size = raw.len() as u32;
        loop {
            match self.net.send_packet(self.socket, meta, raw) {
                Ok(()) => return Ok(()),
                Err(SendError::QueueFull) => {
                    // Our outgoing queue is full; wait for space. This may
                    // also consume a notification for an incoming packet,
                    // which is fine: `serve_forever` drains the incoming
                    // queue before waiting again.
                    sys_recv_notification(self.notification_mask);
                }
                Err(e) => return Err(ServerIoError::Io(NetError::Send(e))),
            }
        }
    }
}

/// An [`nprpc`] server [`Backend`], pairing a [`UdpSocket`] with request and
/// response buffers.
pub struct NetBackend<S: 'static> {
    socket: UdpSocket,
    storage: &'static mut S,
}

impl<S: Storage> NetBackend<S> {
    /// Creates a new backend.
    ///
    /// `storage` is typically a type generated by [`nprpc::autobuffer!`],
    /// held in a `static` so that it doesn't occupy stack space.
    pub fn new(socket: UdpSocket, storage: &'static mut S) -> Self {
        Self { socket, storage }
    }

    /// Serves requests forever.
    ///
    /// `serve_one` is called repeatedly to serve requests, and should
    /// typically call the `serve_one` method of the interface's generated
    /// `Server` trait. When there are no requests waiting this blocks until
    /// the socket's notification is posted.
    ///
    /// Requests that could not be served and errors from the `net` task are
    /// recorded in this crate's ringbuf.
    pub fn serve_forever<F>(&mut self, mut serve_one: F) -> !
    where
        F: FnMut(&mut Self) -> Result<Served, ServerIoError<NetError>>,
    {
        loop {
            match serve_one(self) {
                Ok(Served::Idle) => {
                    sys_recv_notification(self.socket.notification_mask);
                }
                Ok(Served::Responded) => {}
                Ok(Served::Rejected(e)) => {
                    ringbuf_entry!(Trace::Rejected((&e).into()));
                }
                Err(ServerIoError::Server(e)) => {
                    ringbuf_entry!(Trace::Dropped((&e).into()));
                }
                // If `net` restarted, any in-flight packets were lost; there's
                // nothing to do but carry on.
                Err(ServerIoError::Io(e)) => {
                    ringbuf_entry!(Trace::Net(e));
                }
                // `Served` is `non_exhaustive`.
                Ok(_) => {}
                Err(_) => {
                    ringbuf_entry!(Trace::UnknownIoError);
                }
            }
        }
    }
}

impl<S: Storage> Backend for NetBackend<S> {
    type Storage = S;
    type Io = UdpSocket;

    #[inline]
    fn parts(&mut self) -> (&mut Self::Storage, &mut Self::Io) {
        let Self { socket, storage } = self;
        (storage, socket)
    }
}
