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
}

/// Errors returned by the [`minicbor::encode::write::Write`] implementation for
/// [`LeasedWriter`].
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Error {
    /// The other side of the lease has gone away.
    WentAway,
    /// Data could not be written as there was no room left in the lease.
    EndOfLease,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::WentAway => "lease went away",
            Self::EndOfLease => "end of lease",
        })
    }
}

impl<A> minicbor::encode::write::Write for LeasedWriter<'_, A>
where
    A: idol_runtime::AttributeWrite,
{
    type Error = Error;

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        let Some(end) = self.pos.checked_add(buf.len()) else {
            return Err(Error::EndOfLease);
        };
        if end >= self.lease.len() {
            return Err(Error::EndOfLease);
        }
        self.lease
            .write_range(self.pos..end, buf)
            .map_err(|_| Error::WentAway)?;

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
        Self { lease, pos: 0 }
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
}

impl From<Error> for idol_runtime::ClientError {
    fn from(error: Error) -> Self {
        match error {
            Error::EndOfLease => idol_runtime::ClientError::BadLease,
            Error::WentAway => idol_runtime::ClientError::WentAway,
        }
    }
}
