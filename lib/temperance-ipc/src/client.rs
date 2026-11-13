// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

use temperance::io::{
    Storage,
    client::{Backend, Io},
};
use userlib::{Lease, TaskId, extract_new_generation, sys_send};

#[cfg(feature = "leases")]
use crate::SUPPORTED_FLAGS;
use crate::{
    FLAG_RESP_LEASED, FLAG_RQST_LEASED, LEASES, REPLY_OK, REPLY_SERVER_FAILED,
    RESP_LEN_SIZE, operation,
};

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

    /// TODO: Add docs
    #[cfg(feature = "leases")]
    pub fn with_flags(server: TaskId, flags: u16) -> Self {
        Self {
            server,
            flags: flags & SUPPORTED_FLAGS,
        }
    }
}

impl Io for IpcClientIo {
    type Error = IpcClientError;

    fn send_then_receive_raw_frames<'a>(
        &mut self,
        outgoing: &[u8],
        incoming: &'a mut [u8],
    ) -> Result<&'a [u8], Self::Error> {
        let rqst_leased = LEASES && self.flags & FLAG_RQST_LEASED != 0;
        let resp_leased = LEASES && self.flags & FLAG_RESP_LEASED != 0;
        let op = operation(self.flags);
        let mut resp_len = [0u8; RESP_LEN_SIZE];

        let (code, len) = match (rqst_leased, resp_leased) {
            (false, false) => {
                sys_send(self.server, op, outgoing, incoming, &[])
            }
            (true, false) => sys_send(
                self.server,
                op,
                &[],
                incoming,
                &[Lease::read_only(outgoing)],
            ),
            (false, true) => sys_send(
                self.server,
                op,
                outgoing,
                &mut resp_len,
                &[Lease::write_only(incoming)],
            ),
            (true, true) => sys_send(
                self.server,
                op,
                &[],
                &mut resp_len,
                &[Lease::read_only(outgoing), Lease::write_only(incoming)],
            ),
        };

        if code == REPLY_OK {
            let len = if resp_leased {
                if len != RESP_LEN_SIZE {
                    return Err(IpcClientError::BadReplyLength);
                }
                u32::from_le_bytes(resp_len) as usize
            } else {
                len
            };
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

    /// TODO: Add docs
    #[cfg(feature = "leases")]
    pub fn with_flags(server: TaskId, storage: S, flags: u16) -> Self {
        Self {
            io: IpcClientIo::with_flags(server, flags),
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
