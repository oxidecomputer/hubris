// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Validate;
use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::units::Celsius;
use zerocopy::{FromBytes, Immutable, IntoBytes};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The low-level I2C communication returned an error
    I2cError(ResponseCode),
    NoData,
    SensorFailure,
    Reserved,
    InvalidLength,
    BadChecksum,
}

impl From<Error> for ResponseCode {
    fn from(e: Error) -> Self {
        match e {
            Error::I2cError(r) => r,
            Error::NoData
            | Error::SensorFailure
            | Error::Reserved
            | Error::InvalidLength
            | Error::BadChecksum => ResponseCode::BadDeviceState,
        }
    }
}

pub struct NvmeBmc {
    device: I2cDevice,
}

/// See Figure 112: Subsystem Management Data Structure in
/// "NVM Express Management Interface", revision 1.0a, April 8, 2017
#[derive(Copy, Clone, Debug, FromBytes, IntoBytes, Immutable)]
#[repr(C)]
pub struct DriveStatus {
    length: u8,
    flags: u8,
    warnings: u8,
    temperature: u8,
    drive_life_used: u8,
    _reserved: [u8; 2],
    pec: u8,
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

        if v.length != 6 {
            return Err(Error::InvalidLength);
        }

        // Calculate the PEC, which is based on the entire SMBus transaction
        let mut raw_buf: [u8; 10] = [0u8; 10];
        raw_buf[0] = self.device.address << 1;
        raw_buf[2] = (self.device.address << 1) | 1;
        raw_buf[3..].copy_from_slice(&v.as_bytes()[..7]);
        let checksum = smbus_pec::pec(&raw_buf);
        if checksum != v.pec {
            return Err(Error::BadChecksum);
        }

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

impl Validate<ResponseCode> for NvmeBmc {
    fn validate(device: &drv_i2c_api::I2cDevice) -> Result<bool, ResponseCode> {
        // Do a temperature read and see if it works
        let dev = NvmeBmc::new(device);
        let t = dev.read_temperature()?;

        // Confirm that the temperature is not unreasonable
        Ok(t.0 >= 0.0 && t.0 <= 100.0)
    }
}
