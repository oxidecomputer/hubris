// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Functionality shared by multiple models of Murata MWOCP6x power supplies.
//!
//! Currently, this module supports the [`Mwocp68`], used by the Rack Model 0
//! power shelf, and [`Mwocp67`], used by the Rack Model 1 power shelf.

mod mwocp67;
mod mwocp68;
pub use mwocp67::Mwocp67;
pub use mwocp68::Mwocp68;

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

//
// The boot loader command -- sent via BOOT_LOADER_CMD -- is unfortunately odd
// in that its command code is overloaded with BOOT_LOADER_STATUS.  (That is,
// a read to the command code is BOOT_LOADER_STATUS, a write is
// BOOT_LOADER_CMD.)  This is behavior that the PMBus crate didn't necessarily
// envision, so it can't necessarily help us out; we define the single-byte
// payload codes here rather than declaratively in the PMBus crate.
//
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BootLoaderCommand {
    ClearStatus = 0x00,
    RestartProgramming = 0x01,
    BootPrimary = 0x12,
    BootSecondary = 0x02,
    BootPSUFirmware = 0x03,
}

///
/// Defines the state of the firmware update.  Once `UpdateSuccessful`
/// has been returned, the update is complete.
///
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum UpdateState {
    /// The boot loader key has been written
    WroteBootLoaderKey,

    /// The product key has been written
    WroteProductKey,

    /// The boot loader has been booted
    BootedBootLoader,

    /// Programming of firmware has been indicated to have started
    StartedProgramming,

    /// A block has been written; the next offset is at [`offset`], and the
    /// running checksum is in [`checksum`]
    WroteBlock { offset: usize, checksum: u64 },

    /// The last block has been written; the checksum is in [`checksum`]
    WroteLastBlock { checksum: u64 },

    /// The checksum has been sent for verification
    SentChecksum,

    /// The checksum has been verified
    VerifiedChecksum,

    /// The PSU has been rebooted
    RebootedPSU,

    /// The entire update is complete and successful
    UpdateSuccessful,
}

impl UpdateState {
    ///
    /// Return the milliseconds of delay associated with the current state.
    /// Note that some of these values differ slightly from Murata's "PSU
    /// Firmware Update Process" document in that they reflect revised
    /// guidance from Murata.
    ///
    fn delay_ms(&self) -> u64 {
        match self {
            Self::WroteBootLoaderKey => 3_000,
            Self::WroteProductKey => 3_000,
            Self::BootedBootLoader => 1_000,
            Self::StartedProgramming => 2_000,
            Self::WroteBlock { .. } | Self::WroteLastBlock { .. } => 100,
            Self::SentChecksum => 2_000,
            Self::VerifiedChecksum => 4_000,
            Self::RebootedPSU => 5_000,
            Self::UpdateSuccessful => 0,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    BadRead {
        cmd: u8,
        code: ResponseCode,
    },
    BadWrite {
        cmd: u8,
        code: ResponseCode,
    },
    BadData {
        cmd: u8,
    },
    BadValidation {
        cmd: u8,
        code: ResponseCode,
    },
    InvalidData {
        err: pmbus::Error,
    },
    BadFirmwareRevRead {
        code: ResponseCode,
    },
    BadFirmwareRev {
        index: u8,
    },
    BadFirmwareRevLength,
    BadSerialNumberRead {
        code: ResponseCode,
    },
    UpdateInBootLoader,
    UpdateNotInBootLoader,
    UpdateAlreadySuccessful,
    BadBootLoaderStatus {
        data: u8,
    },
    BadBootLoaderCommand {
        cmd: BootLoaderCommand,
        code: ResponseCode,
    },
    ChecksumNotSuccessful,
    BadModelNumberRead {
        code: ResponseCode,
    },
    BadMfrIdRead {
        code: ResponseCode,
    },
    UnsupportedCommand {
        cmd: u8,
    },
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

const FIRMWARE_REVISION_LEN: usize = 14;

/// Returns the firmware revision of the primary MCU, or the index of a parse
/// error.
fn parse_firmware_revision(
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
