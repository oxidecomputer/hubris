// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

use temperance::io::{
    Storage,
    client::{Backend, Io},
};
use userlib::{TaskId, extract_new_generation, sys_send};

use crate::{REPLY_OK, REPLY_SERVER_FAILED, operation};

/// TODO: Add docs
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum IpcClientError {
    /// TODO: Add docs
    ServerDied,
    /// TODO: Add docs
    ServerFailed,
    /// TODO: Add docs
    UnexpectedCode(u32),
    /// TODO: Add docs
    BadReplyLength,
}

/// TODO: Add docs
pub struct IpcClientIo {
    server: TaskId,
    flags: u16,
}

impl IpcClientIo {
    /// TODO: Add docs
    pub fn new(server: TaskId) -> Self {
        Self { server, flags: 0 }
    }
}

impl Io for IpcClientIo {
    type Error = IpcClientError;

    fn send_then_receive_raw_frames<'a>(
        &mut self,
        outgoing: &[u8],
        incoming: &'a mut [u8],
    ) -> Result<&'a [u8], Self::Error> {
        let (code, len) = sys_send(
            self.server,
            operation(self.flags),
            outgoing,
            incoming,
            &[],
        );
        if code == REPLY_OK {
            return incoming.get(..len).ok_or(IpcClientError::BadReplyLength);
        }
        if let Some(g) = extract_new_generation(code) {
            self.server = TaskId::for_index_and_gen(self.server.index(), g);
            return Err(IpcClientError::ServerDied);
        }
        if code == REPLY_SERVER_FAILED {
            return Err(IpcClientError::ServerFailed);
        }
        Err(IpcClientError::UnexpectedCode(code))
    }
}

/// TODO: Add docs
pub struct IpcClient<S> {
    io: IpcClientIo,
    storage: S,
    seqno: u16,
}

impl<S: Storage> IpcClient<S> {
    /// TODO: Add docs
    pub fn new(server: TaskId, storage: S) -> Self {
        Self {
            io: IpcClientIo::new(server),
            storage,
            seqno: 0,
        }
    }
}

impl<S: Storage> Backend for IpcClient<S> {
    type Storage = S;
    type Io = IpcClientIo;

    fn next_sequence_number(&mut self) -> u16 {
        let n = self.seqno;
        self.seqno = n.wrapping_add(1);
        n
    }

    #[inline]
    fn parts(&mut self) -> (&mut Self::Storage, &mut Self::Io) {
        let Self { io, storage, .. } = self;
        (storage, io)
    }
}
