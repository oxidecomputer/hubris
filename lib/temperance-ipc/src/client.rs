// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

use temperance::{
    io::{
        Storage,
        client::{Backend, Io},
    },
    wire::Header,
};
use userlib::{Lease, TaskId, extract_new_generation, sys_send};

use crate::{
    FLAG_RESP_LEASED, FLAG_RQST_LEASED, LEASE_THRESHOLD, LEASES, REPLY_OK,
    REPLY_SERVER_FAILED, RESP_LEN_SIZE, RespLimit, operation,
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
    limits: &'static [RespLimit],
}

impl IpcClientIo {
    /// TODO: Add docs
    pub fn new(server: TaskId, limits: &'static [RespLimit]) -> Self {
        Self { server, limits }
    }

    fn response_limit(&self, outgoing: &[u8]) -> Option<usize> {
        let (hedr, _) = Header::read_from(outgoing).ok()?;
        let key = u64::from_le_bytes(hedr.key.to_bytes());
        self.limits
            .iter()
            .find(|l| l.key == key)
            .and_then(|l| l.max_frame_len)
    }
}

impl Io for IpcClientIo {
    type Error = IpcClientError;

    fn send_then_receive_raw_frames<'a>(
        &mut self,
        outgoing: &[u8],
        incoming: &'a mut [u8],
    ) -> Result<&'a [u8], Self::Error> {
        // TODO: Discuss the fallback to the whole buffer.
        let limit = self
            .response_limit(outgoing)
            .map_or(incoming.len(), |l| l.min(incoming.len()));
        let (incoming, _) = incoming.split_at_mut(limit);

        let rqst_leased = LEASES && outgoing.len() > LEASE_THRESHOLD;
        let resp_leased = LEASES && incoming.len() > LEASE_THRESHOLD;
        let mut flags = 0;
        if rqst_leased {
            flags |= FLAG_RQST_LEASED;
        }
        if resp_leased {
            flags |= FLAG_RESP_LEASED;
        }
        let op = operation(flags);
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
pub struct IpcClient<S: 'static> {
    io: IpcClientIo,
    storage: &'static mut S,
    seqno: u16,
}

impl<S: Storage> IpcClient<S> {
    /// TODO: Add docs
    pub fn new(
        server: TaskId,
        storage: &'static mut S,
        limits: &'static [RespLimit],
    ) -> Self {
        Self {
            io: IpcClientIo::new(server, limits),
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
        (&mut **storage, io)
    }
}
