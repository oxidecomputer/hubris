// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! VPD manipulation

#![no_std]
#![no_main]

use idol_runtime::{RequestError, LeaseBufWriter, LenLimit, Leased, W};
use task_vpd_api::VpdError;
use userlib::*;
use drv_i2c_devices::tmp117::Tmp117;
use drv_i2c_devices::at24csw080::At24Csw080;
use drv_i2c_api::ResponseCode;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

struct ServerImpl;

task_slot!(I2C, i2c_driver);

impl idl::InOrderVpdImpl for ServerImpl {
    fn read_tmp117_eeprom(
        &mut self,
        _: &RecvMessage,
        index: u8,
    ) -> Result<[u8; 6], RequestError<VpdError>> {
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
                Ok(rval) => Ok(rval)
            }
        }
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        index: u8,
        offset: u16,
        dest: LenLimit<Leased<W, [u8]>, 32>,
    ) -> Result<(), RequestError<VpdError>> {
        let devs = i2c_config::devices::at24csw080(I2C.get_task_id());
        let index = index as usize;

        if index >= devs.len() {
            return Err(VpdError::InvalidDevice.into());
        }

        let dev = At24Csw080::new(devs[index]);

        let len = dest.len() as u16;

        if offset + len > drv_i2c_devices::at24csw080::EEPROM_SIZE {
            return Err(VpdError::BadAddress.into());
        }

        if len == 0 {
            return Err(VpdError::BadBuffer.into());
        }

        let mut buf = LeaseBufWriter::<_, 32>::from(dest.into_inner());

        for addr in offset..offset + len {
            if buf.write(match dev.read::<u8>(addr) {
                Err(drv_i2c_devices::at24csw080::Error::I2cError(code)) => {
                    let err: VpdError = code.into();
                    return Err(err.into());
                }

                Err(err) => {
                    panic!();
                }

                Ok(rval) => rval,
            }).is_err() {
                return Ok(());
            }
        }

        Ok(())
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
