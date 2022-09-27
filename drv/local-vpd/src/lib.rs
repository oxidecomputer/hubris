// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use drv_i2c_devices::at24csw080::{At24Csw080, EEPROM_SIZE};
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use userlib::*;
use zerocopy::{AsBytes, FromBytes};

#[derive(Copy, Clone, Debug)]
pub enum LocalVpdError {
    DeviceError,
    NoSuchChunk,
    InvalidChecksum,
    InvalidChunkSize,
}

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

/// Searches for the given tag in the local VPD and reads it
///
/// Returns an error if the tag is not present, the data is of an unexpected
/// size (i.e. not size_of<V>), or any checksum is corrupt.
pub fn read_config<V: Default + AsBytes + FromBytes>(
    i2c_task: TaskId,
    tag: [u8; 4],
) -> Result<V, LocalVpdError> {
    let eeprom = get_vpd_eeprom(i2c_task);
    let eeprom_reader = EepromReader { eeprom: &eeprom };
    let mut reader = TlvcReader::begin(eeprom_reader)
        .map_err(|_| LocalVpdError::DeviceError)?;

    while let Ok(Some(chunk)) = reader.next() {
        if &chunk.header().tag == &tag {
            let mut scratch = [0u8; 32];
            chunk
                .check_body_checksum(&mut scratch)
                .map_err(|_| LocalVpdError::InvalidChecksum)?;

            if chunk.len() as usize != core::mem::size_of::<V>() {
                return Err(LocalVpdError::InvalidChunkSize);
            }

            let mut out = V::default();
            chunk
                .read_exact(0, out.as_bytes_mut())
                .map_err(|_| LocalVpdError::DeviceError)?;
            return Ok(out);
        }
    }
    Err(LocalVpdError::NoSuchChunk.into())
}

include!(concat!(env!("OUT_DIR"), "/vpd_config.rs"));
