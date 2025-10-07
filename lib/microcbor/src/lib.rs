// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! CBOR encoding traits with statically known maximum lengths.
//!
//! This crate provides traits and derive macros for encoding Rust types as
//! CBOR, with the maximum encoded length of the CBOR data determined at compile
//! time. This allows for the maximum buffer size needed to encode the data to
//! be determined at compile-time, allowing static allocation of encoding
//! buffers without the possibility of encoding failures due to insufficient
//! buffer space.
//!
//! When encoding ereports in Hubris, the CBOR messages are generally simple and
//! consist of fixed-size data. However, if buffer sizes for encoding are just
//! chosen arbitrarily by the programmer, it is possible that subsequent changes
//! to the ereport messages will increase the encoded size beyond the chosen
//! buffer size, leading to encoding failures and data loss. Thus, this crate.
//!
//! This crate provides the [`StaticCborLen`] trait for types that can be
//! encoded as CBOR with a known maximum encoded length. In addition, it
//! provides [`#[derive(Encode)`](macro@Encode) and
//! [`#[derive(EncodeFields)`](macro@EncodeFields) derive attributes for
//! deriving implementations of the [`Encode`] and [`StaticCborLen`] traits.
//!
//! ## Wait, Why Not `#[derive(serde::Serialize)]`?
//!
//! Well, the obvious one is that there's no way to know how many bytes a given
//! type's `Serialize` implementation will produce at compile-time.
//!
//! Another limitation, though, is that `serde`'s `#[serde(flatten)]` attribute
//! requires `liballoc`, as the flattened fields are temporarily stored on the
//! heap while encoding. This means that `#[serde(flatten)]` cannot be used to
//! compose nested structs in Hubris ereport messages. By introducing a separate
//! [`EncodeFields`] trait that encodes the fields of a type into a "parent"
//! struct or struct-like enum variant, we avoid this limitation and allow
//! composition of ereport messages.
//!
//! ## Okay, What About `#[derive(minicbor::Encode)]`?
//!
//! `minicbor` also has a first-party derive macro crate, [`minicbor-derive`].
//! Would that be suitable for our purposes? Unfortunately, no.
//! `minicbor-derive` takes a very opinionated approach to CBOR encoding, with
//! the intention of providing forward- and backward-compatibility using a
//! technique similar to the one used by Protocol Buffers`. When using
//! `minicbor-derive`, fields must be annotated with index numbers (like those
//! of protobuf), and fields are serialized with those index numbers as their
//! keys, rather than their actual Rust identifiers.
//!
//! While this scheme is useful in some situations, it doesn't satisfy the goals
//! of the ereport subsystem. The whole reason we are using CBOR in the first
//! place is that we would like to allow decoding key-value data without having
//! to know the structure of the data in advance. `minicbor-derive`'s scheme
//! requires both sides of the exchange to know what field names those index
//! numbers map to at *some* version of the protocol, even if newer versions are
//! backwards compatible. So, it's unsuitable for our purposes.
//!
//! `minicbor-derive` also lacks a way to determine the maximum needed buffer
//! length to encode a value at compile time, which is this crate's primary
//! reason to exist.
//!
//! [`minicbor-derive`]: https://docs.rs/minicbor-derive
#![no_std]
use encode::{Encoder, Write};
#[doc(inline)]
pub use microcbor_derive::{Encode, EncodeFields};
pub use minicbor::encode::{self, Encode};

/// A CBOR-encodable value with a statically-known maximum length.
///
/// A type implementing this trait must implement the [`Encode`]`<()>` trait. In
/// addition, it defines a `MAX_CBOR_LEN` constant that specifies the maximum
/// number of bytes that its [`Encode`] implementation will produce.
pub trait StaticCborLen: Encode<()> {
    /// The maximum length of the CBOR-encoded representation of this value.
    ///
    /// The value is free to encode fewer than this many bytes, but may not
    /// encode more.
    const MAX_CBOR_LEN: usize;
}

/// For a list of types implementing [`StaticCborLen`], returns the maximum length
/// of their CBOR-encoded representations.
///
/// This macro may be used to calculate the maximum buffer size necessary to
/// encode any of a set of types implementing [`StaticCborLen`].
///
/// For example:
///
/// ```rust
///
/// #[derive(microcbor::Encode)]
/// pub struct MyGreatEreport {
///     foo: u32,
///     bar: Option<f64>,
/// }
///
/// #[derive(microcbor::Encode)]
/// pub enum AnotherEreport {
///     A { hello: bool, world: f64 },
///     B(usize),
/// }
///
/// const EREPORT_BUF_LEN: usize = microcbor::max_cbor_len_for![
///     MyGreatEreport,
///     AnotherEreport,
/// ];
///
/// fn main() {
///     let mut ereport_buf = [0; EREPORT_BUF_LEN];
///     // ...
///     # drop(ereport_buf);
/// }
/// ```
#[macro_export]
macro_rules! max_cbor_len_for {
    ($($T:ty),+$(,)?) => {
        {
            let mut len = 0;
            $(
                if <$T as $crate::StaticCborLen>::MAX_CBOR_LEN > len {
                    len = <$T as $crate::StaticCborLen>::MAX_CBOR_LEN;
                }
            )+
            len
        }
    };
}

/// Encode the named fields of a type into an *existing* CBOR map as name-value
/// pairs.
///
/// This is used when a type is included as a field in a "parent" type that
/// derives `EreportData`, and the field in the parent type is annotated with
/// `#[ereport(flatten)]`. When that attribute is present, the fields of the
/// type are encoded as name-value pairs in the parent type's CBOR map, rather
/// than creating a new nested map for the new type being encoded.
///
/// This type may be derived by struct types with named fields, and by enum
/// types where all variants have named fields.
///
/// The [implementation of `EncodeFields` for `Option<T>`][option-impl] will
/// encode the fields of the inner value if it is `Some`, or encode nothing if
/// it is `None`. This way, `#[cbor(flatten)]` may be used with values which are
/// not always present.
///
/// [option-impl]: #impl-EncodeFields<C>-for-Option<T>
pub trait EncodeFields<C> {
    const MAX_FIELDS_LEN: usize;

    fn encode_fields<W: Write>(
        &self,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>>;
}

impl<T, C> EncodeFields<C> for &T
where
    T: EncodeFields<C>,
{
    const MAX_FIELDS_LEN: usize = T::MAX_FIELDS_LEN;

    fn encode_fields<W: Write>(
        &self,
        e: &mut Encoder<W>,
        c: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        T::encode_fields(self, e, c)
    }
}

/// When an `Option<T>` is used as a `#[cbor(flatten)]` field in a type deriving
/// [`Encode`] or [`EncodeFields`], and `T` implements [`EncodeFields`], the
/// `Option` will encode the fields of the inner value if it is `Some`, or
/// encode nothing if it is `None`. This way, `#[cbor(flatten)]` may be used
/// with values which are not always present.
impl<T, C> EncodeFields<C> for Option<T>
where
    T: EncodeFields<C>,
{
    const MAX_FIELDS_LEN: usize = T::MAX_FIELDS_LEN;

    fn encode_fields<W: Write>(
        &self,
        e: &mut Encoder<W>,
        c: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        match self {
            Some(value) => value.encode_fields(e, c),
            None => Ok(()),
        }
    }
}

macro_rules! impl_static_cbor_len {
    ($($T:ty = $len:expr),*$(,)?) => {
        $(
            impl StaticCborLen for $T {
                const MAX_CBOR_LEN: usize = $len;
            }
        )*
    };
}

impl_static_cbor_len! {
    // A u8 may require up to 2 bytes, see:
    // https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#513
    u8 = 2,

    // A u16 may require up to 3 bytes, see:
    // https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#519-523
    u16 = 3,

    // A u32 may require up to 5 bytes, see:
    // https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#529-534
    u32 = 5,

    // A u64 may require up to 9 bytes, see:
    // https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#539-546
    u64 = 9,

    // https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#580
    f32 = 5,

    // https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#586
    f64 = 9,

    // https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#501
    bool = 1,
}

impl StaticCborLen for usize {
    #[cfg(target_pointer_width = "32")]
    const MAX_CBOR_LEN: usize = u32::MAX_CBOR_LEN;

    #[cfg(not(target_pointer_width = "32"))]
    const MAX_CBOR_LEN: usize = u64::MAX_CBOR_LEN;
}

impl<T: StaticCborLen> StaticCborLen for Option<T> {
    const MAX_CBOR_LEN: usize = if T::MAX_CBOR_LEN > 1 {
        T::MAX_CBOR_LEN + 1
    } else {
        1 // always need 1 byte to encode the null, even if T is 0-sized...
    };
}

impl<T: StaticCborLen, const LEN: usize> StaticCborLen for [T; LEN] {
    const MAX_CBOR_LEN: usize = usize_cbor_len(LEN) + (LEN * T::MAX_CBOR_LEN);
}

impl<T: StaticCborLen> StaticCborLen for &T {
    const MAX_CBOR_LEN: usize = T::MAX_CBOR_LEN;
}

pub const fn str_cbor_len(s: &str) -> usize {
    usize_cbor_len(s.len()) + s.len()
}

#[cfg(target_pointer_width = "32")]
pub const fn usize_cbor_len(u: usize) -> usize {
    u32_cbor_len(u as u32)
}

#[cfg(target_pointer_width = "64")]
pub const fn usize_cbor_len(u: usize) -> usize {
    u64_cbor_len(u as u64)
}

pub const fn u32_cbor_len(u: u32) -> usize {
    // https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#529-534
    match u {
        0..=0x17 => 1,
        0x18..=0xff => 2,
        0x100..=0xffff => 3,
        _ => 5,
    }
}

pub const fn u64_cbor_len(u: u64) -> usize {
    // https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#539-546
    match u {
        0..=0x17 => 1,
        0x18..=0xff => 2,
        0x100..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}
