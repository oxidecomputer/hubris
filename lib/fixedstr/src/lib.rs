// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A minimalist fixed-size string type.
//!
//! # Why Not `heapless::String`?
//!
//! The [`heapless::String`] type is also a fixed-length array-backed string
//! type. At a glance, it seems very similar. `FixedStr` is actually somewhat
//! different from `heapless::String`.
//!
//! The `heapless` type provides an API similar to that of
//! `alloc::string::String`, with the ability to push characters/`&str`s at
//! runtime and to mutate the contents of the string. It's designed mainly for
//! uses where you want a mutable string, but cannot allocate it on the heap.
//!
//! Meanwhile, `FixedStr` is mainly intended for use with _immutable_ strings.
//! Unlike `heapless::String`, `FixedStr` does *not* (currently) provide APIs
//! for mutating the contents of the string after it's constructed.[^1] Instead,
//! it has `const fn` [`FixedStr::from_str`], [`FixedStr::try_from_str`], and
//! [`FixedStr::try_from_utf8`] methods, so that a `FixedStr` can be constructed
//! from string or byte literals in a `const` or `static` initializer. While
//! `heapless::String` has a `const fn new`, that function constructs an *empty*
//! string, and the functions that actually push characters to the string are
//! not `const`.
//!
//! [^1]: Because I was too lazy to implement them.
#![no_std]

use core::ops::Deref;

#[derive(Copy, Clone)]
pub struct FixedStr<const MAX: usize> {
    buf: [u8; MAX],
    len: usize,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub struct StringTooLong;

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum FromUtf8Error {
    TooLong,
    InvalidUtf8(core::str::Utf8Error),
}

impl<const MAX: usize> FixedStr<MAX> {
    pub const fn try_from_str(s: &str) -> Result<Self, StringTooLong> {
        let mut buf = [0; MAX];
        let bytes = s.as_bytes();
        let len = bytes.len();
        if len > MAX {
            return Err(StringTooLong);
        }

        // do this instead of `copy_from_slice` so we can be a const fn :/
        let mut idx = 0;
        while idx < len {
            buf[idx] = bytes[idx];
            idx += 1;
        }
        Ok(Self { buf, len })
    }

    pub const fn from_str(s: &str) -> Self {
        match Self::try_from_str(s) {
            Ok(s) => s,
            Err(_) => panic!(),
        }
    }

    pub const fn try_from_utf8(bytes: &[u8]) -> Result<Self, FromUtf8Error> {
        let s = match core::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => return Err(FromUtf8Error::InvalidUtf8(e)),
        };
        match Self::try_from_str(s) {
            Ok(s) => Ok(s),
            Err(StringTooLong) => Err(FromUtf8Error::TooLong),
        }
    }

    pub fn as_str(&self) -> &str {
        unsafe {
            // Safety: we know the buffer up to `self.len` contains valid UTF-8
            // because we only allow this type to be constructed from a `&str`.
            core::str::from_utf8_unchecked(self.as_bytes())
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Converts this `FixedStr` into a byte array.
    ///
    /// The array may be zero-padded if the string is shorter than the maximum
    /// length of the `FixedStr`.
    pub const fn into_array(self) -> [u8; MAX] {
        self.buf
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const MAX: usize> Deref for FixedStr<MAX> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl<const MAX: usize> AsRef<str> for FixedStr<MAX> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<const MAX: usize> AsRef<[u8]> for FixedStr<MAX> {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<const MAX: usize, T> PartialEq<T> for FixedStr<MAX>
where
    T: AsRef<str>,
{
    fn eq(&self, other: &T) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl<const MAX: usize> Eq for FixedStr<MAX> {}

impl<const MAX: usize> core::fmt::Display for FixedStr<MAX> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self.as_str(), f)
    }
}

impl<const MAX: usize> core::fmt::Debug for FixedStr<MAX> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

#[cfg(feature = "microcbor")]
impl<const LEN: usize> microcbor::StaticCborLen for FixedStr<LEN> {
    const MAX_CBOR_LEN: usize = LEN + usize::MAX_CBOR_LEN;
}

#[cfg(any(feature = "minicbor", feature = "microcbor"))]
impl<C, const MAX: usize> minicbor::encode::Encode<C> for FixedStr<MAX> {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::encode::Encoder<W>,
        _: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.str(self.as_str())?;
        Ok(())
    }
}

#[cfg(feature = "serde")]
impl<const MAX: usize> serde::Serialize for FixedStr<MAX> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[cfg(feature = "serde")]
impl<'de, const MAX: usize> serde::Deserialize<'de> for FixedStr<MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ExpectedLen(usize);
        impl serde::de::Expected for ExpectedLen {
            fn fmt(
                &self,
                f: &mut core::fmt::Formatter<'_>,
            ) -> core::fmt::Result {
                write!(f, "a string of length <= {} bytes", self.0)
            }
        }
        let s = <&'de str>::deserialize(deserializer)?;
        Self::try_from_str(s).map_err(|_: StringTooLong| {
            serde::de::Error::invalid_length(s.len(), &ExpectedLen(MAX))
        })
    }
}
