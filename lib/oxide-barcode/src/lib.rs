// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Parsing VPD barcode strings.

#![cfg_attr(not(test), no_std)]

use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
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
    BadRevision,
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
pub struct VpdIdentity {
    pub part_number: [u8; Self::PART_NUMBER_LEN],
    pub revision: u32,
    pub serial: [u8; Self::SERIAL_LEN],
}

impl VpdIdentity {
    pub const PART_NUMBER_LEN: usize = 11;
    pub const SERIAL_LEN: usize = 11;
}

impl VpdIdentity {
    pub fn parse(barcode: &[u8]) -> Result<Self, ParseError> {
        let mut fields = barcode.split(|&b| b == b':');

        let version = fields.next().ok_or(ParseError::MissingVersion)?;
        let part_number = fields.next().ok_or(ParseError::MissingPartNumber)?;
        let revision = fields.next().ok_or(ParseError::MissingRevision)?;
        let serial = fields.next().ok_or(ParseError::MissingSerial)?;
        if fields.next().is_some() {
            return Err(ParseError::UnexpectedFields);
        }

        // Note: the fact that this is created _zeroed_ is important for the
        // variable length field handling below.
        let mut out = VpdIdentity::new_zeroed();

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
        // tail is already zeroed

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn check_parse(input: &[u8], expected: VpdIdentity) {
        assert_eq!(
            expected,
            VpdIdentity::parse(input).unwrap(),
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
            VpdIdentity::parse(&copy).unwrap(),
            "parsing string: {}",
            String::from_utf8_lossy(&copy),
        );
    }

    #[test]
    fn parse_oxv1() {
        check_parse(
            b"0XV1:1230000456:023:TST01234567",
            VpdIdentity {
                part_number: *b"123-0000456",
                revision: 23,
                serial: *b"TST01234567",
            },
        );
    }

    #[test]
    fn parse_oxv2() {
        check_parse(
            b"0XV2:123-0000456:023:TST01234567",
            VpdIdentity {
                part_number: *b"123-0000456",
                revision: 23,
                serial: *b"TST01234567",
            },
        );
    }

    #[test]
    fn parse_oxv2_shorter_serial() {
        check_parse(
            b"0XV2:123-0000456:023:TST0123456",
            VpdIdentity {
                part_number: *b"123-0000456",
                revision: 23,
                // should get padded with NULs to the right:
                serial: *b"TST0123456\0",
            },
        );
    }

    #[test]
    fn parse_oxv2_shorter_part() {
        check_parse(
            b"0XV2:123-000045:023:TST01234567",
            VpdIdentity {
                // should get padded with NULs to the right:
                part_number: *b"123-000045\0",
                revision: 23,
                serial: *b"TST01234567",
            },
        );
    }
}
