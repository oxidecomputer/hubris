// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use drv_i2c_devices::at24csw080::{At24Csw080, EEPROM_SIZE};
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use userlib::*;
use zerocopy::{AsBytes, LittleEndian, U16};

#[derive(Copy, Clone, Debug)]
pub enum LocalVpdError {
    DeviceError,
    NoSuchChunk,
}

pub fn read_config(
    i2c_task: TaskId,
    tag: [u8; 4],
    out: &mut [u8],
) -> Result<(), LocalVpdError> {
    #[derive(Clone)]
    struct EepromReader<'a> {
        eeprom: &'a At24Csw080,
    }
    impl<'a> TlvcRead for EepromReader<'a> {
        fn extent(&self) -> Result<u64, TlvcReadError> {
            Ok(EEPROM_SIZE as u64)
        }
        fn read_exact(
            &self,
            offset: u64,
            dest: &mut [u8],
        ) -> Result<(), TlvcReadError> {
            self.eeprom
                .read_into(offset as u16, dest)
                .map_err(|_| TlvcReadError::Truncated)?;
            Ok(())
        }
    }

    let eeprom = get_vpd_eeprom(i2c_task);
    let eeprom_reader = EepromReader { eeprom: &eeprom };
    let mut reader = TlvcReader::begin(eeprom_reader)
        .map_err(|_| LocalVpdError::DeviceError)?;

    while let Ok(Some(chunk)) = reader.next() {
        if &chunk.header().tag == &tag {
            let mut base_mac = [0u8; 6];
            chunk
                .read_exact(0, &mut base_mac)
                .map_err(|_| LocalVpdError::DeviceError)?;

            let mut count: U16<LittleEndian> = U16::new(0);
            chunk
                .read_exact(6, count.as_bytes_mut())
                .map_err(|_| LocalVpdError::DeviceError)?;

            let mut stride = 0u8;
            chunk
                .read_exact(8, stride.as_bytes_mut())
                .map_err(|_| LocalVpdError::DeviceError)?;

            return Ok(());
        }
    }
    Err(LocalVpdError::NoSuchChunk.into())
}

include!(concat!(env!("OUT_DIR"), "/vpd_config.rs"));
