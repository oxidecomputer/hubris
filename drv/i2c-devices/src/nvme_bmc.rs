// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::units::Celsius;
use zerocopy::{AsBytes, FromBytes};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The low-level I2C communication returned an error
    I2cError(ResponseCode),
    NoData,
    SensorFailure,
    Reserved,
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::I2cError(code) => code,
            _ => todo!(),
        }
    }
}

pub struct NvmeBmc {
    device: I2cDevice,
}

/// See Figure 112: Subsystem Management Data Structure in
/// "NVM Express Management Interface", revision 1.0a, April 8, 2017
#[derive(Copy, Clone, Debug, FromBytes, AsBytes)]
#[repr(C)]
pub struct DriveStatus {
    length: u8,
    flags: u8,
    warnings: u8,
    temperature: u8,
    drive_life_used: u8,
    _reserved: [u8; 2],
}

impl NvmeBmc {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }
    pub fn read_temperature(&self) -> Result<Celsius, Error> {
        let v = self
            .device
            .read_reg::<u8, DriveStatus>(0)
            .map_err(Error::I2cError)?;
        // Again, see Figure 112 in "NVM Express Management Interface",
        // revision 1.0a, April 8, 2017
        match v.temperature {
            0..=0x7E => Ok(Celsius(v.temperature as f32)),
            0x7F => Ok(Celsius(127.0)),
            0xC4 => Ok(Celsius(-60.0)),
            0x80 => Err(Error::NoData),
            0x81 => Err(Error::SensorFailure),
            0x82..=0xC3 => Err(Error::Reserved),

            // Cast to i8, since this is a two's complement value
            0xC5..=0xFF => Ok(Celsius((v.temperature as i8) as f32)),
        }
    }
}
