// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! VPD manipulation

#![no_std]
#![no_main]

use drv_i2c_devices::at24csw080::{At24Csw080, EEPROM_SIZE};
use idol_runtime::RequestError;
use task_vpd_api::VpdError;
use userlib::*;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

struct ServerImpl;

task_slot!(I2C, i2c_driver);

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
                    let code: drv_i2c_api::ResponseCode = err.into();
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

        match dev.write::<u8>(offset, contents) {
            Err(drv_i2c_devices::at24csw080::Error::I2cError(code)) => {
                let err: VpdError = code.into();
                Err(err.into())
            }

            Err(_) => Err(VpdError::BadWrite.into()),

            Ok(rval) => Ok(rval),
        }
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
