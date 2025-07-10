// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An adapter implementing [`minicbor::encode::write::Write`] for
//! [`idol_runtime::Leased`] byte buffers.

#![no_std]

/// An adapter implementing [`minicbor::encode::write::Write`] for
/// [`idol_runtime::Leased`] byte buffers.
pub struct LeasedWriter<A> {
    lease: idol_runtime::Leased<A, [u8]>,
    pos: usize,
}

#[derive(Copy, Clone, PartialEq)]
pub enum Error {
    WentAway,
    EndOfLease,
}

impl minicbor::encode::write::Write for LeasedWriter<A>
where
    A: idol_runtime::AttributeWrite,
{
    type Error = Error;

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        let end = self.pos + buf.len();
        if end >= self.lease.len() {
            return Error::EndOfLease;
        }
        self.lease
            .write_range(self.pos..end, buf)
            .map_err(|_| Error::WentAway)?;

        self.pos += buf.len();

        Ok(())
    }
}

impl<A> LeasedWriter<A>
where
    A: idol_runtime::AttributeWrite,
{
    /// Returns a new `LeasedWriter` starting at byte 0 of the lease.
    pub fn new(lease: idol_runtime::Leased<A, [u8]>) -> Self {
        Self { lease, pos: 0 }
    }

    /// Returns a new `LeasedWriter` starting at the specified position in the
    /// lease.
    ///
    /// This is intended for cases where some data has already been written to
    /// the lease.
    pub fn starting_at(lease: idol_runtime::Leased<A, [u8]>) -> Self {
        Self { lease, pos: 0 }
    }

    /// Returns the current byte position within the lease.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Returns the underlying lease, consuming the writer.
    pub fn into_inner(self) -> idol_runtime::Leased<A, [u8]> {
        self.lease
    }
}
