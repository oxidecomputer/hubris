// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the VPD task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, LittleEndian, U16,
};

pub use gateway_ereport_messages as ereport_messages;
pub use host_sp_messages::HostStartupOptions;
pub use oxide_barcode::VpdIdentity;

/// Represents a range of allocated MAC addresses, per RFD 320
///
/// The SP will claim the first `N` addresses based on VLAN configuration
/// (typically either 1 or 2).
#[derive(
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    FromBytes,
    IntoBytes,
    Immutable,
    KnownLayout,
    Default,
)]
#[repr(C)]
pub struct MacAddressBlock {
    pub base_mac: [u8; 6],
    pub count: U16<LittleEndian>,
    pub stride: u8,
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum CacheGetError {
    ValueNotSet = 1,
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum CacheSetError {
    ValueAlreadySet = 1,
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum EreportReadError {
    RestartIdNotSet = 1,
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum EreportWriteError {
    /// Indicates that an ereport was lost because it would not have fit in
    /// Packrat's ereport buffer.
    Lost = 1,
}

/// Errors returned by [`Packrat::serialize_ereport`].
#[derive(counters::Count)]
#[cfg(feature = "serde")]
pub enum EreportSerializeError {
    /// The IPC to deliver the serialized ereport failed.
    Packrat {
        len: usize,
        #[count(children)]
        err: EreportWriteError,
    },
    /// Serializing the ereport failed.
    Serialize(
        minicbor_serde::error::EncodeError<minicbor::encode::write::EndOfSlice>,
    ),
}

#[cfg(feature = "serde")]
impl Packrat {
    /// Deliver an ereport for a value that implements [`serde::Serialize`]. The
    /// provided `buf` is used to serialize the value before sending it to
    /// Packrat.
    pub fn serialize_ereport(
        &self,
        ereport: &impl serde::Serialize,
        buf: &mut [u8],
    ) -> Result<usize, EreportSerializeError> {
        let mut s = {
            let writer = minicbor::encode::write::Cursor::new(buf);
            minicbor_serde::Serializer::new(writer)
        };

        // Try to serialize the ereport...
        ereport
            .serialize(&mut s)
            .map_err(EreportSerializeError::Serialize)?;

        // Okay, get the buffer back out, and figure out how much of it was
        // used.
        let writer = s.into_encoder().into_writer();
        let len = writer.position();
        let buf = writer.into_inner();

        // Now, try to send that to Packrat.
        self.deliver_ereport(&buf[..len])
            .map_err(|err| EreportSerializeError::Packrat { len, err })?;

        Ok(len)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
