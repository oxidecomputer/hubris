// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Functionality shared by multiple models of Murata MWOCP6x power supplies.
//!
//! Currently, this module supports the [`Mwocp68`], used by the Rack Model 0
//! power shelf, and [`Mwocp67`], used by the Rack Model 1 power shelf.

// TODO don't separately re-export device struct? reconsider structure here
pub use crate::mwocp67::{self, Mwocp67};
pub use crate::mwocp68::{self, Mwocp68};

use crate::BadValidation;
use drv_i2c_api::ResponseCode;

/// The revision of the firmware on a Murata PSU's MCU.
#[derive(Copy, Clone, PartialEq)]
pub struct FirmwareRev(pub [u8; 4]);

/// The unique serial number of a Murata PSU.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub struct SerialNumber(pub [u8; 12]);

/// Manufacturer model number.
///
/// Per Murata Application Note ACAN-114.A01.D03 "PMBus Communication Protocol",
/// this is always a 17-byte ASCII string. It should be "MWOCP68-3600-D-RM" or
/// "MWOCP67-5500-B-RM".
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub struct ModelNumber(pub [u8; 17]);

/// Manufacturer ID.
///
/// Per Murata Application Note ACAN-114.A01.D03 "PMBus Communication Protocol",
/// this is always a 9-byte ASCII string. It should be "Murata-PS".
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub struct MfrId(pub [u8; 9]);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadData { cmd: u8 },
    BadValidation { cmd: u8, code: ResponseCode },
    InvalidData { err: pmbus::Error },
    BadFirmwareRevRead { code: ResponseCode },
    BadFirmwareRev { index: u8 },
    BadFirmwareRevLength,
    BadSerialNumberRead { code: ResponseCode },
    UpdateInBootLoader,
    UpdateNotInBootLoader,
    UpdateAlreadySuccessful,
    BadBootLoaderStatus { data: u8 },
    BadBootLoaderCommand { cmd: u8, code: ResponseCode },
    ChecksumNotSuccessful,
    BadModelNumberRead { code: ResponseCode },
    BadMfrIdRead { code: ResponseCode },
    UnsupportedCommand { cmd: u8 },
}

impl From<BadValidation> for Error {
    fn from(value: BadValidation) -> Self {
        Self::BadValidation {
            cmd: value.cmd,
            code: value.code,
        }
    }
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRead { code, .. } => code,
            Error::BadWrite { code, .. } => code,
            Error::BadValidation { code, .. } => code,
            _ => ResponseCode::BadDeviceState,
        }
    }
}

impl From<pmbus::Error> for Error {
    fn from(err: pmbus::Error) -> Self {
        Error::InvalidData { err }
    }
}

pub(crate) const FIRMWARE_REVISION_LEN: usize = 14;

/// Returns the firmware revision of the primary MCU, or the index of a parse
/// error.
pub(crate) fn parse_firmware_revision(
    data: &[u8; FIRMWARE_REVISION_LEN],
) -> Result<FirmwareRev, u8> {
    // Per ACAN-114 and ACAN-157, we are expecting this to be of the format:
    //
    //    XXXX-YYYY-0000
    //
    // Where XXXX is the firmware revision on the primary MCU (AC input
    // side) and YYYY is the firmware revision on the secondary MCU (DC
    // output side).  We aren't going to be rigid about the format of
    // either revision, but we will be rigid about the rest of the format.
    let expected = b"XXXX-YYYY-0000";
    for index in 0..expected.len() {
        if expected[index] == b'X' || expected[index] == b'Y' {
            continue;
        }

        if data[index] != expected[index] {
            return Err(index as u8);
        }
    }

    // Return the primary MCU version
    Ok(FirmwareRev([data[0], data[1], data[2], data[3]]))
}
