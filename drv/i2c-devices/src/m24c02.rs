// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the M24C02-WMN6TP EEPROM attached to a MWOCP68 power shelf

use crate::Validate;
use drv_i2c_api::{I2cDevice, ResponseCode};

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadValidation { code: ResponseCode },
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRead { code, .. } => code,
            Error::BadWrite { code, .. } => code,
            Error::BadValidation { code, .. } => code,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct M24C02 {
    device: I2cDevice,
}

impl M24C02 {
    pub fn read_eeprom(&self) -> Result<[u8; 256], ResponseCode> {
        self.device.read_reg::<u8, _>(0)
    }
}

impl Validate<Error> for M24C02 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        // Attempt to read a byte at address 0
        //
        // TODO: actually check against an expected pattern
        device
            .read_reg::<u8, u8>(0)
            .map_err(|e| Error::BadValidation { code: e })?;
        Ok(true)
    }
}
