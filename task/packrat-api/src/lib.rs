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

/// A utility type for encoding ereport class strings constructed from multiple
/// `&str` segments.
///
/// This type implements `minicbor::encode::Encode` for an array of `&str`s,
/// encoding the array as a single CBOR string consisting of all the segments
/// joined together, delimited by `.` characters.
///
/// This is intended to reduce the number of bytes in a Hubris task's binary
/// when multiple ereport class strings with shared segments may be emitted. For
/// example, suppose a task will report the following ereport classes:
///
/// - `foo.bar.baz`
/// - `foo.bar.baz.quux`
/// - `foo.bar.womble`
///
/// If these class strings are all represented as `&'static str`s, then the
/// binary will contain three copies of the string `"foo.bar."`, using 24 bytes,
/// and an additional two copies of `baz`, for 6 additional bytes.
///
/// Using `EreportClass`, the individual segments can be represented as
/// `&'static str`s, and joined together depending on the class of the ereport.
/// For example:
///
/// ```rust
/// # use task_packrat_api::EreportClass;
/// static CLASS_FOO: &'static str = "foo";
/// static CLASS_BAR: &'static str = "bar";
/// static CLASS_BAZ: &'static str = "baz";
///
/// let foo_bar_baz = EreportClass(&[CLASS_FOO, CLASS_BAR, CLASS_BAZ]);
/// let foo_bar_baz_quux = EreportClass(&[CLASS_FOO, CLASS_BAR, CLASS_BAZ, "quux"]);
/// let foo_bar_womble = EreportClass(&[CLASS_FOO, CLASS_BAR, "womble]);
/// // ... use these classes when recording ereports ...
/// # drop((foo_bar_baz, foo_bar_baz_quux, foo_bar_womble))
/// ```
///
/// This way, only a single copy of "foo", "bar", and "baz" make it into the
/// binary, saving us 15 bytes of flash. Of course, this saving is more
/// pronounced when more class strings, with longer segments, are used. Also, as
/// the `.` separator is added by `EreportClass` when encoding the class string,
/// duplicate `.` characters also don't make it into the binary.
#[derive(Copy, Clone)]
pub struct EreportClass<'a>(pub &'a [&'a str]);

impl<C> minicbor::Encode<C> for EreportClass<'_> {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // TODO(eliza): would prefer to represent this as one big length
        // prefixed string, since we should be able to calculate that, but need
        // `minicbor` v2.1.1 for `str_len`...
        let mut wrote_any_segments = false;
        e.begin_str()?;
        for segment in self.0 {
            if wrote_any_segments {
                e.str(".")?;
            }
            e.str(segment)?;
            wrote_any_segments = true;
        }
        e.end()?;
        Ok(())
    }
}

#[cfg(feature = "serde")]
pub struct SerdeEreport<'a, T> {
    pub class: &'a EreportClass<'a>,
    pub data: &'a T,
}

#[cfg(feature = "serde")]
impl<T> SerdeEreport<'_, T>
where
    T: serde::Serialize,
{
    pub fn to_writer<W>(
        &self,
        writer: W,
    ) -> Result<W, minicbor_serde::error::EncodeError<W::Error>>
    where
        W: minicbor::encode::Write,
        W::Error: core::error::Error + 'static,
    {
        let mut s = minicbor_serde::Serializer::new(writer);
        s.encoder_mut()
            .map(2)?
            .str("k")?
            .encode(self.class)?
            .str("data")?;
        self.data.serialize(&mut s)?;
        Ok(s.into_encoder().into_writer())
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
