// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! [`nprpc`] `Io`, `Storage`, and `Backend` implementations over byte streams.

use std::io::{BufRead, BufReader, StdinLock, StdoutLock, Write};
use std::process::Child;

use nprpc::io::server::{RawIoFrame, ServerIoError};
use nprpc::io::{Storage, StorageView, client, server};

use crate::frame::{read_frame, write_frame};

/// Transport-level errors.
#[derive(Debug)]
pub enum IoError {
    Io(std::io::Error),
    /// The stream ended, either between frames (the peer went away) or in the
    /// middle of one.
    Eof,
    /// A frame did not decode as COBS.
    Decode,
    /// A frame was larger than the receive buffer.
    TooLarge,
}

impl From<std::io::Error> for IoError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Eof => write!(f, "peer closed the stream"),
            Self::Decode => {
                write!(f, "received a frame that is not valid COBS")
            }
            Self::TooLarge => {
                write!(f, "received a frame larger than the buffer")
            }
        }
    }
}

impl std::error::Error for IoError {}

/// A reader/writer pair carrying COBS-framed [`nprpc`] frames.
///
/// Implements the client `Io` (send a request, wait for the response) and the
/// server `Io` (wait for a request, send a response) so the same type serves
/// both ends.
pub struct StreamIo<R, W> {
    reader: R,
    writer: W,
}

impl<R: BufRead, W: Write> StreamIo<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }
}

impl<R: BufRead, W: Write> client::Io for StreamIo<R, W> {
    type Error = IoError;

    fn send_then_receive_raw_frames<'a>(
        &mut self,
        outgoing: &[u8],
        incoming: &'a mut [u8],
    ) -> Result<&'a [u8], IoError> {
        write_frame(&mut self.writer, outgoing)?;
        read_frame(&mut self.reader, incoming)?.ok_or(IoError::Eof)
    }
}

impl<R: BufRead, W: Write> server::Io for StreamIo<R, W> {
    type Meta = ();
    type Error = IoError;

    fn recv_one_frame_raw<'data>(
        &mut self,
        incoming: &'data mut [u8],
    ) -> Result<Option<RawIoFrame<'data, ()>>, ServerIoError<IoError>> {
        // End of stream is reported as an error rather than `None`: `None`
        // would make `serve_one` return success with nothing served, and a
        // server loop could never tell that the client is gone.
        let raw = read_frame(&mut self.reader, incoming)
            .map_err(ServerIoError::Io)?
            .ok_or(ServerIoError::Io(IoError::Eof))?;
        Ok(Some(RawIoFrame { meta: (), raw }))
    }

    fn send_one_frame_raw(
        &mut self,
        outgoing: RawIoFrame<'_, ()>,
    ) -> Result<(), ServerIoError<IoError>> {
        write_frame(&mut self.writer, outgoing.raw).map_err(ServerIoError::Io)
    }
}

/// Default size of each of the request and response buffers.
pub const DEFAULT_BUFFER_LEN: usize = 1 << 20;

/// Heap-allocated request and response buffers.
///
/// The interfaces carry unbounded byte vectors, so `nprpc`'s automatically
/// sized buffers don't apply; a frame larger than these buffers fails with
/// [`IoError::TooLarge`].
pub struct HeapStorage {
    rqst: Vec<u8>,
    resp: Vec<u8>,
}

impl HeapStorage {
    pub fn new(len: usize) -> Self {
        Self {
            rqst: vec![0; len],
            resp: vec![0; len],
        }
    }
}

impl Default for HeapStorage {
    fn default() -> Self {
        Self::new(DEFAULT_BUFFER_LEN)
    }
}

impl Storage for HeapStorage {
    fn buffers(&mut self) -> StorageView<'_> {
        StorageView {
            rqst_buf: &mut self.rqst,
            resp_buf: &mut self.resp,
        }
    }
}

/// The task side: an `nprpc` client backend.
///
/// Bring the `Client` traits of [`syscalls`](crate::syscalls) and
/// [`runtime`](crate::runtime) into scope to call methods on it.
pub struct Client<R, W> {
    storage: HeapStorage,
    io: StreamIo<R, W>,
    seqno: u16,
}

impl<R: BufRead, W: Write> Client<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            storage: HeapStorage::default(),
            io: StreamIo::new(reader, writer),
            seqno: 0,
        }
    }
}

impl Client<StdinLock<'static>, StdoutLock<'static>> {
    /// A client over the process's own stdin and stdout, which it locks for
    /// its lifetime. Nothing else in the process may print to stdout.
    pub fn stdio() -> Self {
        Self::new(std::io::stdin().lock(), std::io::stdout().lock())
    }
}

impl<R: BufRead, W: Write> client::Backend for Client<R, W> {
    type Storage = HeapStorage;
    type Io = StreamIo<R, W>;

    fn next_sequence_number(&mut self) -> u16 {
        let n = self.seqno;
        self.seqno = self.seqno.wrapping_add(1);
        n
    }

    fn parts(&mut self) -> (&mut HeapStorage, &mut StreamIo<R, W>) {
        (&mut self.storage, &mut self.io)
    }
}

/// The fixture side: an `nprpc` server backend.
///
/// Pass it to `serve_one` of a type implementing the
/// [`all::Server`](crate::all::Server) trait.
pub struct Server<R, W> {
    storage: HeapStorage,
    io: StreamIo<R, W>,
}

impl<R: BufRead, W: Write> Server<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            storage: HeapStorage::default(),
            io: StreamIo::new(reader, writer),
        }
    }
}

impl Server<BufReader<std::process::ChildStdout>, std::process::ChildStdin> {
    /// A server over a spawned task process, which must have been started
    /// with both `stdin` and `stdout` piped.
    pub fn for_child(child: &mut Child) -> Option<Self> {
        let stdout = child.stdout.take()?;
        let stdin = child.stdin.take()?;
        Some(Self::new(BufReader::new(stdout), stdin))
    }
}

impl<R: BufRead, W: Write> server::Backend for Server<R, W> {
    type Storage = HeapStorage;
    type Io = StreamIo<R, W>;

    fn parts(&mut self) -> (&mut HeapStorage, &mut StreamIo<R, W>) {
        (&mut self.storage, &mut self.io)
    }
}
