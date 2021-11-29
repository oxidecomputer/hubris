// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Host Flash server.

#![no_std]

use core::cell::Cell;
use userlib::*;
use zerocopy::AsBytes;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq)]
pub enum Operation {
    ReadId = 1,
    ReadStatus = 2,
    BulkErase = 3,
    PageProgram = 4,
    Read = 5,
    SectorErase = 6,
}

/// Errors that can be produced from the host flash server API.
///
/// This enumeration doesn't include errors that result from configuration
/// issues, like sending host flash messages to some other task.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
pub enum HfError {
    WriteEnableFailed = 1,
    ServerRestarted = 2,
}

impl From<HfError> for u32 {
    fn from(rc: HfError) -> Self {
        rc as u32
    }
}

/// Errors that can be produced from the host flash server itself. This is a
/// supserset of `HfError` including cases that should not be capable of
/// occurring if the client is correct.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum InternalHfError {
    Recoverable(HfError),
    BadMessage,
    MissingLease,
    BadLease,
}

impl From<InternalHfError> for u32 {
    fn from(rc: InternalHfError) -> Self {
        match rc {
            InternalHfError::Recoverable(e) => u32::from(e),
            // These need to be larger than anything in HfError.
            InternalHfError::BadMessage => 0x1000,
            InternalHfError::MissingLease => 0x1001,
            InternalHfError::BadLease => 0x1002,
        }
    }
}

impl From<HfError> for InternalHfError {
    fn from(e: HfError) -> Self {
        Self::Recoverable(e)
    }
}

#[derive(Clone, Debug)]
pub struct HostFlash(Cell<TaskId>);

impl From<TaskId> for HostFlash {
    fn from(t: TaskId) -> Self {
        Self(Cell::new(t))
    }
}

impl HostFlash {
    /// Reads the 20-byte Device ID data from the host flash.
    pub fn read_id(&self, out: &mut [u8; 20]) -> Result<(), HfError> {
        let n = self.send(Operation::ReadId, &[], out, &[])?;
        assert!(n == 20);
        Ok(())
    }

    /// Reads the host flash chip's Status Register.
    pub fn read_status(&self) -> Result<u8, HfError> {
        let mut status = 0;
        let n =
            self.send(Operation::ReadStatus, &[], status.as_bytes_mut(), &[])?;
        assert!(n == 1);
        Ok(status)
    }

    /// Issues a bulk erase command to the host flash and waits for it to
    /// complete. Note that this can take a rather long time.
    pub fn bulk_erase(&self) -> Result<(), HfError> {
        self.send(Operation::BulkErase, &[], &mut [], &[])?;
        Ok(())
    }

    /// Issues a page program command to the host flash, writing `data` starting
    /// at `address`.
    pub fn page_program(
        &self,
        address: u32,
        data: &[u8],
    ) -> Result<(), HfError> {
        self.send(
            Operation::PageProgram,
            address.as_bytes(),
            &mut [],
            &[Lease::from(data)],
        )?;
        Ok(())
    }

    /// Reads from the host flash starting at `address` into `data`.
    pub fn read(&self, address: u32, data: &mut [u8]) -> Result<(), HfError> {
        self.send(
            Operation::Read,
            address.as_bytes(),
            &mut [],
            &[Lease::from(data)],
        )?;
        Ok(())
    }

    /// Issues a sector erase command to the host flash, for the 64kiB sector
    /// containing `address`.
    pub fn sector_erase(&self, address: u32) -> Result<(), HfError> {
        self.send(Operation::SectorErase, address.as_bytes(), &mut [], &[])?;
        Ok(())
    }

    fn send(
        &self,
        operation: Operation,
        outgoing: &[u8],
        incoming: &mut [u8],
        leases: &[Lease<'_>],
    ) -> Result<usize, HfError> {
        let task = self.0.get();

        let (rc, rlen) =
            sys_send(task, operation as u16, outgoing, incoming, leases);

        // Detect truncated response messages.
        assert!(rlen <= incoming.len());
        // Detect error codes.
        if rc == 0 {
            Ok(rlen)
        } else if let Some(g) = abi::extract_new_generation(rc) {
            // Detect server death and update task, but do not retry.
            self.0.set(TaskId::for_index_and_gen(task.index(), g));
            Err(HfError::ServerRestarted)
        } else if let Some(err) = HfError::from_u32(rc) {
            Err(err)
        } else {
            // Unexpected error code from server is some sort of configuration
            // error that we can't reasonably recover from.
            panic!()
        }
    }
}
