// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An adapter implementing [`minicbor::encode::write::Write`] for
//! [`idol_runtime::Leased`] byte buffers.

#![no_std]

/// An adapter implementing [`minicbor::encode::write::Write`] for
/// [`idol_runtime::Leased`] byte buffers.
pub struct LeasedWriter<'lease, A> {
    lease: &'lease mut idol_runtime::Leased<A, [u8]>,
    pos: usize,
    ran_out_of_space: bool,
}

#[derive(Copy, Clone, PartialEq)]
pub enum Error {
    WentAway,
    EndOfLease,
}

impl minicbor::encode::write::Write for LeasedWriter<'_, A>
where
    A: idol_runtime::AttributeWrite,
{
    type Error = Error;

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        let end = self.pos + buf.len();
        if end >= self.lease.len() {
            self.ran_out_of_space = true;
            return Err(Error::EndOfLease);
        }
        self.lease
            .write_range(self.pos..end, buf)
            .map_err(|_| Error::WentAway)?;

        self.pos += buf.len();

        Ok(())
    }
}

impl<A> LeasedWriter<'_, A>
where
    A: idol_runtime::AttributeWrite,
{
    /// Returns a new `LeasedWriter` starting at byte 0 of the lease.
    pub fn new(lease: &mut idol_runtime::Leased<A, [u8]>) -> Self {
        Self { lease, pos: 0 }
    }

    /// Returns a new `LeasedWriter` starting at the specified position in the
    /// lease.
    ///
    /// This is intended for cases where some data has already been written to
    /// the lease.
    pub fn starting_at(lease: &mut idol_runtime::Leased<A, [u8]>) -> Self {
        Self { lease, pos: 0 }
    }

    /// Returns the current byte position within the lease.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Returns the underlying lease, consuming the writer.
    pub fn into_inner(self) -> &mut idol_runtime::Leased<A, [u8]> {
        self.lease
    }

    /// Returns `true` if the last `write_all` call to return an error failed
    /// due to running out of space.
    ///
    /// This is an unfortunate workaround for a limitation of the `minicbor`
    /// API: the errors our `Write` implementation can return are wrapped in an
    /// `encode::Error`, and we can't actually get our errors *out* of that
    /// wrapper, so there's no way to tell whether the error was becasue we ran
    /// out of space in the buffer, or because the lease client went away. So,
    /// we track it here, so that the caller can just ask us if we didn't have
    /// space to encode something.
    ///
    /// Note that this is separate from `pos == lease.len()`, because we don't
    /// actually write anything (and thus don't advance `pos`) if asked to write
    /// a chunk of bytes that don't fit.
    pub fn ran_out_of_space(&self) -> bool {
        self.ran_out_of_space
    }
}
