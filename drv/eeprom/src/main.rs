// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver task for the AT24CSW080 EEPROM
//!
//! This is an _extremely minimal_ driver, meant to be used from Humility
//! for extremely basic bring-up testing.
//!
//! When using the EEPROM in production, it will be owned by a more
//! featureful task which will use the I2C device directly from
//! `i2c_config::devices::at24csw080_*`.

#![no_std]
#![no_main]

use derive_idol_err::IdolError;
use drv_i2c_devices::at24csw080::*;
use idol_runtime::{NotificationHandler, RequestError};
use userlib::{task_slot, FromPrimitive};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
task_slot!(I2C, i2c_driver);

/// The `EepromError` is a simple `enum` that copies the more detailed
/// `drv_i2c_devices::at24csw080::Error` type, discarding extra data
/// so this can be sent in Idol messages.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, IdolError)]
#[repr(u32)]
pub enum EepromError {
    I2cError = 1,
    InvalidAddress,
    InvalidEndAddress,
    InvalidObjectSize,
    MisalignedPage,
    InvalidPageSize,
    InvalidSecurityRegisterReadByte,
    InvalidSecurityRegisterWriteByte,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<Error> for EepromError {
    fn from(err: Error) -> Self {
        match err {
            Error::I2cError(_) => Self::I2cError,
            Error::InvalidAddress(_) => Self::InvalidAddress,
            Error::InvalidEndAddress(_) => Self::InvalidEndAddress,
            Error::InvalidObjectSize(_) => Self::InvalidObjectSize,
            Error::MisalignedPage(_) => Self::MisalignedPage,
            Error::InvalidPageSize(_) => Self::InvalidPageSize,
            Error::InvalidSecurityRegisterReadByte(_) => {
                Self::InvalidSecurityRegisterReadByte
            }
            Error::InvalidSecurityRegisterWriteByte(_) => {
                Self::InvalidSecurityRegisterWriteByte
            }
        }
    }
}

struct EepromServer {
    dev: At24Csw080,
}

/// The simplest possible EEPROM implementation.
///
/// This is _very inefficient_; writing byte-by-byte incurs a 5ms pause after
/// each byte, which could be reduced by writing 16-byte pages.
impl idl::InOrderEepromImpl for EepromServer {
    fn read_byte(
        &mut self,
        _msg: &userlib::RecvMessage,
        addr: u16,
    ) -> Result<u8, RequestError<EepromError>> {
        self.dev
            .read::<u8>(addr)
            .map_err(|e| EepromError::from(e).into())
    }

    fn write_byte(
        &mut self,
        _msg: &userlib::RecvMessage,
        addr: u16,
        value: u8,
    ) -> Result<(), RequestError<EepromError>> {
        self.dev
            .write_byte(addr, value)
            .map_err(|e| EepromError::from(e).into())
    }
}

impl NotificationHandler for EepromServer {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    let i2c_task = I2C.get_task_id();
    let dev =
        At24Csw080::new(i2c_config::devices::at24csw080_local(i2c_task)[0]);
    let mut srv = EepromServer { dev };
    let mut buffer = [0u8; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut srv);
    }
}

mod idl {
    use super::EepromError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
