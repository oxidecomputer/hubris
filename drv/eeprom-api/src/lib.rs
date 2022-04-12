// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for EEPROM driver.

#![no_std]

use derive_idol_err::IdolError;
use drv_i2c_devices::at24csw080::Error as RawError;
use userlib::*;

/// The `EepromError` is a simple `enum` that copies the more detailed
/// `drv_i2c_devices::at24csw080::Error` type, discarding extra data
/// so this can be sent in Idol messages.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, IdolError)]
#[repr(u32)]
pub enum EepromError {
    I2cError = 1,
    InvalidAddress = 2,
    InvalidEndAddress = 3,
    InvalidObjectSize = 4,
    MisalignedPage = 5,
    InvalidPageSize = 6,
    InvalidSecurityRegisterReadByte = 7,
    InvalidSecurityRegisterWriteByte = 8,
}

impl From<RawError> for EepromError {
    fn from(err: RawError) -> Self {
        match err {
            RawError::I2cError(_) => Self::I2cError,
            RawError::InvalidAddress(_) => Self::InvalidAddress,
            RawError::InvalidEndAddress(_) => Self::InvalidEndAddress,
            RawError::InvalidObjectSize(_) => Self::InvalidObjectSize,
            RawError::MisalignedPage(_) => Self::MisalignedPage,
            RawError::InvalidPageSize(_) => Self::InvalidPageSize,
            RawError::InvalidSecurityRegisterReadByte(_) => {
                Self::InvalidSecurityRegisterReadByte
            }
            RawError::InvalidSecurityRegisterWriteByte(_) => {
                Self::InvalidSecurityRegisterWriteByte
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
