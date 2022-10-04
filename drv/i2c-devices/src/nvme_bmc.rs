// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::units::Celsius;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The low-level I2C communication returned an error
    I2cError(ResponseCode),
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::I2cError(code) => code,
        }
    }
}

pub struct NvmeBmc {
    device: I2cDevice,
}

impl NvmeBmc {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }
    pub fn read_temperature(&self) -> Result<Celsius, Error> {
        todo!()
    }
}
