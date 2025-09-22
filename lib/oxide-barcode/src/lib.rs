// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Parsing VPD barcode strings.

#![cfg_attr(not(test), no_std)]

use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use zerocopy::{
    FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    MissingVersion,
    MissingPartNumber,
    MissingRevision,
    MissingSerial,
    UnexpectedFields,
    UnknownVersion,
    WrongPartNumberLength,
    WrongSerialLength,
    WrongMpn1Length,
    BadRevision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    BufferTooSmall,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, SerializedSize,
)]
#[repr(u8)]
pub enum VpdIdentity {
    Oxide(OxideIdentity),
    Mpn1(Mpn1Identity),
}

pub trait ParseBarcode: Sized {
    fn parse_barcode(barcode: &[u8]) -> Result<Self, ParseError>;
}

impl VpdIdentity {
    pub fn parse(barcode: &[u8]) -> Result<Self, ParseError> {
        let mut fields = barcode.split(|&b| b == b':');

        let version = fields.next().ok_or(ParseError::MissingVersion)?;
        match version {
            b"MPN1" => Mpn1Identity::parse(barcode).map(VpdIdentity::Mpn1),
            _ => OxideIdentity::from_parts(version, fields)
                .map(VpdIdentity::Oxide),
        }
    }
}

impl ParseBarcode for VpdIdentity {
    fn parse_barcode(barcode: &[u8]) -> Result<Self, ParseError> {
        VpdIdentity::parse(barcode)
    }
}

#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    FromBytes,
    IntoBytes,
    Unaligned,
    Immutable,
    KnownLayout,
    Serialize,
    Deserialize,
    SerializedSize,
)]
#[repr(C, packed)]
pub struct OxideIdentity {
    pub part_number: [u8; Self::PART_NUMBER_LEN],
    pub revision: u32,
    pub serial: [u8; Self::SERIAL_LEN],
}

impl OxideIdentity {
    pub const PART_NUMBER_LEN: usize = 11;
    pub const SERIAL_LEN: usize = 11;
    const OXV2: &'static [u8] = b"0XV2:";
    const MAX_LEN: usize =
        Self::OXV2.len() + Self::PART_NUMBER_LEN + Self::SERIAL_LEN
        + 3 // revision part
        + 2 // delimiters
        ;

    pub fn encode_into(&self, buf: &mut [u8]) -> Result<usize, EncodeError> {
        fn write_chunk(offset: &mut usize, buf: &mut [u8], data: &[u8]) {
            // if a serial or part number was shorter than 11 characters, it may
            // be nul-padded. handle that by chopping off any nuls.
            let len =
                data.iter().position(|b| *b == b'\0').unwrap_or(data.len());
            buf[*offset..*offset + len].copy_from_slice(&data[..len]);
            *offset += len;
        }

        if buf.len() < Self::MAX_LEN {
            return Err(EncodeError::BufferTooSmall);
        }

        let mut offset = 0;
        write_chunk(&mut offset, buf, Self::OXV2);
        write_chunk(&mut offset, buf, &self.part_number[..]);
        write_chunk(&mut offset, buf, b":");
        // Encode revision
        {
            use core::fmt::Write;
            // Sadly, `std::io::Cursor` is not in libcore, so we have to
            // reimplement just enough of it to `fmt::Write` the revision lol
            // lmao
            struct WriteThingy<'a> {
                buf: &'a mut [u8],
                pos: usize,
            }
            impl Write for WriteThingy<'_> {
                fn write_str(&mut self, s: &str) -> core::fmt::Result {
                    let bytes = s.as_bytes();

                    if bytes.len() > self.buf.len() - self.pos {
                        return Err(core::fmt::Error);
                    }
                    self.buf[self.pos..self.pos + bytes.len()]
                        .copy_from_slice(bytes);
                    self.pos += bytes.len();
                    Ok(())
                }
            }
            let buf = &mut buf[offset..offset + 4];
            let rev = self.revision;
            write!(&mut WriteThingy { buf, pos: 0 }, "{rev:03}:")
                .map_err(|_| EncodeError::BufferTooSmall)?;
            offset += 4
        }

        write_chunk(&mut offset, buf, &self.serial[..]);
        Ok(offset)
    }

    pub fn parse(barcode: &[u8]) -> Result<Self, ParseError> {
        let mut fields = barcode.split(|&b| b == b':');

        let version = fields.next().ok_or(ParseError::MissingVersion)?;
        Self::from_parts(version, fields)
    }

    fn from_parts<'parts>(
        version: &'parts [u8],
        mut fields: impl Iterator<Item = &'parts [u8]> + 'parts,
    ) -> Result<Self, ParseError> {
        let part_number = fields.next().ok_or(ParseError::MissingPartNumber)?;
        let revision = fields.next().ok_or(ParseError::MissingRevision)?;
        let serial = fields.next().ok_or(ParseError::MissingSerial)?;
        if fields.next().is_some() {
            return Err(ParseError::UnexpectedFields);
        }

        // Note: the fact that this is created _zeroed_ is important for the
        // variable length field handling below.
        let mut out = OxideIdentity::new_zeroed();

        match version {
            // V1 does not include the hyphen in the part number; we need to
            // insert it.
            b"OXV1" | b"0XV1" => {
                if part_number.len() != out.part_number.len() - 1 {
                    return Err(ParseError::WrongPartNumberLength);
                }
                out.part_number[..3].copy_from_slice(&part_number[..3]);
                out.part_number[3] = b'-';
                out.part_number[4..].copy_from_slice(&part_number[3..]);
            }
            // V2 part number includes the hyphen; copy it as-is.
            b"OXV2" | b"0XV2" => {
                if part_number.len() > out.part_number.len() {
                    return Err(ParseError::WrongPartNumberLength);
                }
                out.part_number[..part_number.len()]
                    .copy_from_slice(part_number);
                // tail is already zeroed due to use of new_zeroed above
            }
            _ => return Err(ParseError::UnknownVersion),
        }

        out.revision = core::str::from_utf8(revision)
            .ok()
            .and_then(|rev| rev.parse().ok())
            .ok_or(ParseError::BadRevision)?;

        if serial.len() > out.serial.len() {
            return Err(ParseError::WrongSerialLength);
        }
        out.serial[..serial.len()].copy_from_slice(serial);

        Ok(out)
    }
}

impl ParseBarcode for OxideIdentity {
    fn parse_barcode(barcode: &[u8]) -> Result<Self, ParseError> {
        OxideIdentity::parse(barcode)
    }
}

/// A barcode in the [`MPN1` format].
///
/// Unlike the `OXV1` and `OXV2` formats, the part number, revision, and serial
/// number portions of this format are all variable-length. The only length
/// limits in this format are that the manufacturer code should be three
/// characters, and the total length of the barcode cannot exceed 128 bytes.
/// Therefore, rather than parsing the barcode into a structure where the
/// individual portions are stored in their own fixed size arrays, as in the
/// [`OxideIdentity`] struct, we represent MPN1 barcodes as a single 128-byte
/// array.
///
/// [`MPN1` format]: https://rfd.shared.oxide.computer/rfd/308#fmt-mpn
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    FromBytes,
    IntoBytes,
    Unaligned,
    Immutable,
    KnownLayout,
    Serialize,
    Deserialize,
    SerializedSize,
)]
#[repr(C, packed)]
pub struct Mpn1Identity {
    #[serde(with = "BigArray")]
    pub buf: [u8; Self::MAX_LEN],
    pub len: u8,
}

impl Mpn1Identity {
    pub const MAX_LEN: usize = 128;

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len()]
    }

    pub fn mfg(&self) -> Option<&[u8]> {
        self.nth_part(0)
    }

    pub fn mpn(&self) -> Option<&[u8]> {
        self.nth_part(1)
    }

    pub fn revision(&self) -> Option<&[u8]> {
        self.nth_part(2)
    }

    pub fn serial(&self) -> Option<&[u8]> {
        self.nth_part(3)
    }

    fn nth_part(&self, n: usize) -> Option<&[u8]> {
        let part = self.bytes().split(|b| *b == b':').nth(n)?;
        if part.is_empty() {
            None
        } else {
            Some(part)
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (version, rest) = bytes
            .split_at_checked(5)
            .ok_or(ParseError::MissingVersion)?;
        if version != b"MPN1:" {
            return Err(ParseError::UnknownVersion);
        }
        Self::from_bytes(rest)
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ParseError> {
        let len = bytes.len();

        let mut buf = [0u8; Self::MAX_LEN];
        buf.get_mut(..len)
            .ok_or(ParseError::WrongMpn1Length)?
            .copy_from_slice(bytes);
        Ok(Self {
            buf,
            // Note: casting to u8 here is fine since the length has just been
            // checked to be less than 128.
            len: bytes.len() as u8,
        })
    }
}

impl ParseBarcode for Mpn1Identity {
    fn parse_barcode(barcode: &[u8]) -> Result<Self, ParseError> {
        Mpn1Identity::parse(barcode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn check_parse_oxide(input: &[u8], expected: OxideIdentity) {
        assert_eq!(
            expected,
            OxideIdentity::parse(input).unwrap(),
            "parsing string: {}",
            String::from_utf8_lossy(input),
        );

        // We accept barcode strings that start with both leading zero and
        // leading capital-O. Permute our input from one of these to the other
        // to make sure both forms parse equivalently.
        let mut copy = input.to_vec();
        match copy[0] {
            b'0' => copy[0] = b'O',
            b'O' => copy[0] = b'0',
            c => {
                panic!("unexpected leading character: {}", c as char)
            }
        }

        assert_eq!(
            expected,
            OxideIdentity::parse(&copy).unwrap(),
            "parsing string: {}",
            String::from_utf8_lossy(&copy),
        );
    }

    #[test]
    fn parse_oxv1() {
        check_parse_oxide(
            b"0XV1:1230000456:023:TST01234567",
            OxideIdentity {
                part_number: *b"123-0000456",
                revision: 23,
                serial: *b"TST01234567",
            },
        );
    }

    #[test]
    fn parse_oxv2() {
        check_parse_oxide(
            b"0XV2:123-0000456:023:TST01234567",
            OxideIdentity {
                part_number: *b"123-0000456",
                revision: 23,
                serial: *b"TST01234567",
            },
        );
    }

    #[test]
    fn parse_oxv2_shorter_serial() {
        check_parse_oxide(
            b"0XV2:123-0000456:023:TST0123456",
            OxideIdentity {
                part_number: *b"123-0000456",
                revision: 23,
                // should get padded with NULs to the right:
                serial: *b"TST0123456\0",
            },
        );
    }

    #[test]
    fn parse_oxv2_shorter_part() {
        check_parse_oxide(
            b"0XV2:123-000045:023:TST01234567",
            OxideIdentity {
                // should get padded with NULs to the right:
                part_number: *b"123-000045\0",
                revision: 23,
                serial: *b"TST01234567",
            },
        );
    }

    #[track_caller]
    fn check_reencode_oxide(input: &[u8]) {
        let parsed = match OxideIdentity::parse(input) {
            Ok(parsed) => parsed,
            Err(e) => panic!(
                "failed to parse {:?}: {e:?}",
                String::from_utf8_lossy(input),
            ),
        };

        let mut expected = [0u8; OxideIdentity::MAX_LEN];
        expected[..input.len()].copy_from_slice(input);

        let mut reencoded = [0u8; OxideIdentity::MAX_LEN];
        match parsed.encode_into(&mut reencoded) {
            Ok(_) => (),
            Err(e) => panic!(
                "failed to encode {:?}: {e:?}",
                String::from_utf8_lossy(input),
            ),
        };

        assert_eq!(
            expected,
            reencoded,
            "re-encoded string \"{}\" does not match original \"{}\"",
            String::from_utf8_lossy(&reencoded),
            String::from_utf8_lossy(&expected),
        )
    }

    #[test]
    fn reencode_oxv2() {
        check_reencode_oxide(b"0XV2:123-0000456:023:TST01234567");
    }

    #[test]
    fn reencode_oxv2_shorter_serial() {
        check_reencode_oxide(b"0XV2:123-0000456:023:TST0123456");
    }

    #[test]
    fn reencode_xv2_shorter_part() {
        check_reencode_oxide(b"0XV2:123-000045:023:TST01234567");
    }

    #[track_caller]
    fn check_parse_mpn1(
        input: &[u8],
        mfg: Option<&[u8]>,
        mpn: Option<&[u8]>,
        rev: Option<&[u8]>,
        serial: Option<&[u8]>,
    ) {
        match VpdIdentity::parse(input) {
            Ok(VpdIdentity::Mpn1(id)) => {
                assert_eq!(
                    id.mfg(),
                    mfg,
                    "parsing MPN1 identity {:?} MFG",
                    String::from_utf8_lossy(input),
                );
                assert_eq!(
                    id.mpn(),
                    mpn,
                    "parsing MPN1 identity {:?} MPN",
                    String::from_utf8_lossy(input),
                );
                assert_eq!(
                    id.revision(),
                    rev,
                    "parsing MPN1 identity {:?} REV",
                    String::from_utf8_lossy(input),
                );
                assert_eq!(
                    id.serial(),
                    serial,
                    "parsing MPN1 identity {:?} SERIAL",
                    String::from_utf8_lossy(input),
                );
            }
            Ok(VpdIdentity::Oxide(id)) => {
                panic!(
                    "expected MPN1 identity {:?}, but parsed as OXV1/OXV2: {id:?}",
                    String::from_utf8_lossy(input)
                );
            }
            Err(e) => {
                panic!(
                    "parsing MPN1 identity {:?}: {e:?}",
                    String::from_utf8_lossy(input),
                );
            }
        }
    }

    #[test]
    fn parse_mpn1() {
        let input = b"MPN1:ABC:ASDF-1000:032:123456789";
        check_parse_mpn1(
            input,
            Some(b"ABC"),
            Some(b"ASDF-1000"),
            Some(b"032"),
            Some(b"123456789"),
        );
    }

    #[test]
    fn parse_mpn1_empty() {
        let input = b"MPN1::::";
        check_parse_mpn1(input, None, None, None, None);
    }

    #[test]
    fn parse_mpn1_no_mpn_rev() {
        let input = b"MPN1:XYZ:::12345ABCD";
        check_parse_mpn1(input, Some(b"XYZ"), None, None, Some(b"12345ABCD"));
    }

    #[test]
    fn parse_mpn1_no_serial() {
        let input = b"MPN1:XYZ:1234ABC:420:";
        check_parse_mpn1(
            input,
            Some(b"XYZ"),
            Some(b"1234ABC"),
            Some(b"420"),
            None,
        );
    }

    #[test]
    fn mpn1_serde_roundtrip() {
        let input = b"MPN1:ABC:ASDF-1000:032:123456789";
        let vpd = VpdIdentity::parse(input).expect("MPN1 should parse");
        let mut buf = [0u8; VpdIdentity::MAX_SIZE];
        let len = dbg!(hubpack::serialize(&mut buf, &vpd))
            .expect("serialization should succeed");
        let (vpd2, _) = dbg!(hubpack::deserialize(&buf[..len]))
            .expect("deserialization should succeed");
        assert_eq!(vpd, vpd2);
    }
}
