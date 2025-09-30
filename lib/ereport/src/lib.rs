// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hubris ereport traits.
//!
//! ## Wait, Why Not `#[derive(serde::Serialize)]`?
//!
//! TODO ELIZA EXPLAIN
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
//! [`minicbor-derive`]: https://docs.rs/minicbor-derive
#![no_std]

use encode::{Encoder, Write};
pub use ereport_derive::EreportData;
pub use minicbor::encode::{self, Encode};

pub trait EreportData: Encode<()> {
    /// The maximum length of the CBOR-encoded representation of this value.
    ///
    /// The value is free to encode fewer than this many bytes, but may not
    /// encode more.
    const MAX_CBOR_LEN: usize;
}

#[macro_export]
macro_rules! max_cbor_len_for {
    ($($T:ty),+$(,)?) => {
        {
            let mut len = 0;
            $(
                if <$T as $crate::EreportData>::MAX_CBOR_LEN > len {
                    len = <$T as $crate::EreportData>::MAX_CBOR_LEN;
                }
            )+
            len
        }
    };
}

pub trait EncodeFields<C> {
    const MAX_FIELDS_LEN: usize;

    fn encode_fields<W: Write>(
        &self,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>>;
}

#[cfg(feature = "fixedstr")]
impl<const LEN: usize> EreportData for fixedstr::FixedStr<LEN> {
    const MAX_CBOR_LEN: usize = LEN + usize::MAX_CBOR_LEN;
}

macro_rules! impl_ereport_data {
    ($($T:ty = $len:expr),*$(,)?) => {
        $(
            impl EreportData for $T {
                const MAX_CBOR_LEN: usize = $len;
            }
        )*
    };
}

impl_ereport_data! {
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

    //https://docs.rs/minicbor/2.1.1/src/minicbor/encode.rs.html#501
    bool = 1,
}

impl EreportData for usize {
    #[cfg(target_pointer_width = "32")]
    const MAX_CBOR_LEN: usize = u32::MAX_CBOR_LEN;

    #[cfg(not(target_pointer_width = "32"))]
    const MAX_CBOR_LEN: usize = u64::MAX_CBOR_LEN;
}

impl<T: EreportData> EreportData for Option<T> {
    const MAX_CBOR_LEN: usize = if T::MAX_CBOR_LEN > 1 {
        T::MAX_CBOR_LEN + 1
    } else {
        1 // always need 1 byte to encode the null, even if T is 0-sized...
    };
}

impl<T: EreportData, const LEN: usize> EreportData for [T; LEN] {
    const MAX_CBOR_LEN: usize = usize_cbor_len(LEN) + (LEN * T::MAX_CBOR_LEN);
}

impl<T: EreportData> EreportData for &T {
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
