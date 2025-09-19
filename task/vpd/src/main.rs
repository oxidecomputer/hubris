// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! VPD manipulation

#![no_std]
#![no_main]

use drv_i2c_devices::at24csw080::{At24Csw080, EEPROM_SIZE};
use idol_runtime::{NotificationHandler, RequestError};
use task_vpd_api::VpdError;
use userlib::*;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

struct ServerImpl;

task_slot!(I2C, i2c_driver);

fn eeprom_is_locked(
    dev: &drv_i2c_devices::at24csw080::At24Csw080,
) -> Result<bool, RequestError<VpdError>> {
    use drv_i2c_devices::at24csw080::WriteProtectBlock;

    match dev.read_eeprom_write_protect() {
        Err(drv_i2c_devices::at24csw080::Error::I2cError(code)) => {
            let err: VpdError = code.into();
            Err(err.into())
        }
        Err(_) => Err(VpdError::DeviceError.into()),
        Ok(wp) if wp.locked => match wp.block {
            Some(WriteProtectBlock::AllMemory) => Ok(true),
            _ => Err(VpdError::PartiallyLocked.into()),
        },
        Ok(_) => Ok(false),
    }
}

impl idl::InOrderVpdImpl for ServerImpl {
    #[cfg(feature = "tmp117-eeprom")]
    fn read_tmp117_eeprom(
        &mut self,
        _: &RecvMessage,
        index: u8,
    ) -> Result<[u8; 6], RequestError<VpdError>> {
        use drv_i2c_api::ResponseCode;
        use drv_i2c_devices::tmp117::Tmp117;

        let devs = i2c_config::devices::tmp117(I2C.get_task_id());
        let index = index as usize;

        if index >= devs.len() {
            Err(VpdError::InvalidDevice.into())
        } else {
            let dev = Tmp117::new(&devs[index]);

            match dev.read_eeprom() {
                Err(err) => {
                    let code: ResponseCode = err.into();
                    let err: VpdError = code.into();
                    Err(err.into())
                }
                Ok(rval) => Ok(rval),
            }
        }
    }

    #[cfg(not(feature = "tmp117-eeprom"))]
    fn read_tmp117_eeprom(
        &mut self,
        _: &RecvMessage,
        _index: u8,
    ) -> Result<[u8; 6], RequestError<VpdError>> {
        Err(VpdError::NotImplemented.into())
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        index: u8,
        offset: u16,
    ) -> Result<[u8; 16], RequestError<VpdError>> {
        const LEN: usize = 16;

        let devs = i2c_config::devices::at24csw080(I2C.get_task_id());
        let index = index as usize;

        if index >= devs.len() {
            return Err(VpdError::InvalidDevice.into());
        }

        let dev = At24Csw080::new(devs[index]);

        if offset as usize + LEN > EEPROM_SIZE as usize {
            return Err(VpdError::BadAddress.into());
        }

        match dev.read::<[u8; LEN]>(offset) {
            Err(drv_i2c_devices::at24csw080::Error::I2cError(code)) => {
                let err: VpdError = code.into();
                Err(err.into())
            }

            Err(_) => Err(VpdError::BadRead.into()),

            Ok(rval) => Ok(rval),
        }
    }

    fn write(
        &mut self,
        _: &RecvMessage,
        index: u8,
        offset: u16,
        contents: u8,
    ) -> Result<(), RequestError<VpdError>> {
        let devs = i2c_config::devices::at24csw080(I2C.get_task_id());
        let index = index as usize;

        if index >= devs.len() {
            return Err(VpdError::InvalidDevice.into());
        }

        let dev = At24Csw080::new(devs[index]);

        if offset as usize + 1 > EEPROM_SIZE as usize {
            return Err(VpdError::BadAddress.into());
        }

        if eeprom_is_locked(&dev)? {
            return Err(VpdError::IsLocked.into());
        }

        match dev.write::<u8>(offset, contents) {
            Err(drv_i2c_devices::at24csw080::Error::I2cError(code)) => {
                let err: VpdError = code.into();
                Err(err.into())
            }

            Err(_) => Err(VpdError::BadWrite.into()),

            Ok(rval) => Ok(rval),
        }
    }

    fn is_locked(
        &mut self,
        _: &RecvMessage,
        index: u8,
    ) -> Result<bool, RequestError<VpdError>> {
        let devs = i2c_config::devices::at24csw080(I2C.get_task_id());
        let index = index as usize;

        if index >= devs.len() {
            return Err(VpdError::InvalidDevice.into());
        }

        let dev = At24Csw080::new(devs[index]);
        eeprom_is_locked(&dev)
    }

    fn permanently_lock(
        &mut self,
        _: &RecvMessage,
        index: u8,
    ) -> Result<(), RequestError<VpdError>> {
        let devs = i2c_config::devices::at24csw080(I2C.get_task_id());
        let index = index as usize;

        if index >= devs.len() {
            return Err(VpdError::InvalidDevice.into());
        }

        let dev = At24Csw080::new(devs[index]);

        if eeprom_is_locked(&dev)? {
            return Err(VpdError::AlreadyLocked.into());
        }

        let all = drv_i2c_devices::at24csw080::WriteProtectBlock::AllMemory;

        //
        // Full send!
        //
        match dev.permanently_enable_eeprom_write_protection(all) {
            Err(drv_i2c_devices::at24csw080::Error::I2cError(code)) => {
                let err: VpdError = code.into();
                Err(err.into())
            }

            Err(_) => Err(VpdError::BadLock.into()),
            Ok(()) => Ok(()),
        }
    }

    fn num_vpd_devices(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<core::convert::Infallible>> {
        Ok(i2c_config::devices::at24csw080(I2C.get_task_id()).len())
    }
}

impl NotificationHandler for ServerImpl {
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
    let mut server = ServerImpl;
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::VpdError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
