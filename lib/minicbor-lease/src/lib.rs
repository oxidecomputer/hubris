// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An adapter implementing [`minicbor::encode::write::Write`] for
//! [`idol_runtime::Leased`] byte buffers.

#![no_std]

/// An adapter implementing [`minicbor::encode::write::Write`] for
/// [`idol_runtime::Leased`] byte buffers.
pub struct LeasedWriter<'lease, A>
where
    A: idol_runtime::AttributeWrite,
{
    lease: &'lease mut idol_runtime::Leased<A, [u8]>,
    pos: usize,
    ran_out_of_space: bool,
}

/// Errors returned by [`LeasedWriter::check_err`].
#[derive(Copy, Clone, PartialEq)]
pub enum Error {
    /// The other side of the lease has gone away.
    WentAway,
    /// Data could not be written as there was no room left in the lease.
    EndOfLease,
}

/// Errors returned by the [`minicbor::encode::write::Write`] implementation for
/// [`LeasedWriter`].
#[derive(Copy, Clone, PartialEq)]
pub struct WriteError(());

impl<A> minicbor::encode::write::Write for LeasedWriter<'_, A>
where
    A: idol_runtime::AttributeWrite,
{
    type Error = WriteError;

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        let end = self.pos + buf.len();
        if end >= self.lease.len() {
            self.ran_out_of_space = true;
            return Err(WriteError(()));
        }
        self.lease
            .write_range(self.pos..end, buf)
            .map_err(|_| WriteError(()))?;

        self.pos += buf.len();

        Ok(())
    }
}

impl<'lease, A> LeasedWriter<'lease, A>
where
    A: idol_runtime::AttributeWrite,
{
    /// Returns a new `LeasedWriter` starting at byte 0 of the lease.
    pub fn new(lease: &'lease mut idol_runtime::Leased<A, [u8]>) -> Self {
        Self {
            lease,
            pos: 0,
            ran_out_of_space: false,
        }
    }

    /// Returns a new `LeasedWriter` starting at the specified byte position in
    /// the lease.
    ///
    /// This is intended for cases where some data has already been written to
    /// the lease.
    pub fn starting_at(
        position: usize,
        lease: &'lease mut idol_runtime::Leased<A, [u8]>,
    ) -> Self {
        Self {
            lease,
            pos: position,
            ran_out_of_space: false,
        }
    }

    /// Returns the current byte position within the lease.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Borrows the underlying lease from the writer.
    pub fn lease(&self) -> &idol_runtime::Leased<A, [u8]> {
        self.lease
    }

    /// Returns the underlying lease, consuming the writer.
    pub fn into_inner(self) -> &'lease mut idol_runtime::Leased<A, [u8]> {
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

    /// Determine whether an error returned by the [`minicbor::encode::Write`]
    /// implementation indicates that there was no space left in the lease, or
    /// that the client went away.
    ///
    /// This is an unfortunate workaround for a limitation of the `minicbor`
    /// API: the errors our `Write` implementation can return are wrapped in an
    /// `encode::Error`, and we can't actually get our errors *out* of that
    /// wrapper, so there's no way to tell whether the error was becasue we ran
    /// out of space in the buffer, or because the lease client went away. So,
    /// we track it here, so that the caller can just ask us if we didn't have
    /// space to encode something.
    ///
    pub fn check_err(&self, err: minicbor::encode::Error<WriteError>) -> Error {
        if err.is_write() {
            if self.ran_out_of_space {
                Error::EndOfLease
            } else {
                Error::WentAway
            }
        } else {
            // This is an encoder error, which we have no good way to
            // recover from.
            panic!()
        }
    }
}

impl From<Error> for idol_runtime::ClientError {
    fn from(error: Error) -> Self {
        match error {
            Error::EndOfLease => idol_runtime::ClientError::BadLease,
            Error::WentAway => idol_runtime::ClientError::WentAway,
        }
    }
}
