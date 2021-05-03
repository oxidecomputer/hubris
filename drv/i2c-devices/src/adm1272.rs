//! Driver for the ADM1272 hot-swap controller

use drv_i2c_api::*;
use drv_pmbus::*;

pub struct Adm1272 {
    device: I2cDevice,
}

impl core::fmt::Display for Adm1272 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "adm1272: {}", &self.device)
    }
}

impl Adm1272 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    pub fn read_manufacturer(
        &self,
        buf: &mut [u8],
    ) -> Result<(), ResponseCode> {
        self.device.read_block(Command::ManufacturerID as u8, buf)
    }

    pub fn read_model(&self, buf: &mut [u8]) -> Result<(), ResponseCode> {
        self.device
            .read_block(Command::ManufacturerModel as u8, buf)
    }
}
