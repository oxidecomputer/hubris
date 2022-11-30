// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

//! Driver to read vital product data (VPD) from the local FRU ID EEPROM.
//!
//! The *local* EEPROM is the one soldered to the PCB itself; the system may
//! have additional EEPROMs on FRUs that plug into the board (e.g. fans), but
//! those are *not handled* by this driver. We assume that the local EEPROM is
//! an AT24CSW080, and that it contains keys in TLV-C format (see RFD 148 for a
//! general description, or RFD 320 for the specific example of MAC addresses)
//!
//! The app TOML file must have one AT24xx named `local_vpd`; we use that name
//! to pick which EEPROM to read.

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
    NoRootChunk,
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

/// Searches for the given TLV-C tag in the local VPD and reads it
///
/// Returns an error if the tag is not present, the data is of an unexpected
/// size (i.e. not size_of<V>), or any checksum is corrupt.
///
/// The data in the EEPROM is assumed to be of the form
/// ```ron
/// ("FRU0", [
///     ("TAG1", [ [...] ]),
///     ("TAG2", [ [...] ]),
///     ("TAG3", [ [...] ]),
/// ])
/// ```
/// (where `TAG*` are example tags)
///
/// `read_config` should be called with a tag nested under `FRU0` (e.g. `TAG1`
/// in the example above).  It will deserialize the raw byte array (shown as
/// `[...]`) into an object of type `V`.
pub fn read_config<V: AsBytes + FromBytes>(
    i2c_task: TaskId,
    tag: [u8; 4],
) -> Result<V, LocalVpdError> {
    let eeprom = drv_i2c_devices::at24csw080::At24Csw080::new(
        i2c_config::devices::at24csw080_local_vpd(i2c_task),
    );
    let eeprom_reader = EepromReader { eeprom: &eeprom };
    let mut reader = TlvcReader::begin(eeprom_reader)
        .map_err(|_| LocalVpdError::DeviceError)?;

    while let Ok(Some(chunk)) = reader.next() {
        let mut scratch = [0u8; 32];
        if chunk.header().tag == *b"FRU0" {
            chunk
                .check_body_checksum(&mut scratch)
                .map_err(|_| LocalVpdError::InvalidChecksum)?;
            let mut inner = chunk.read_as_chunks();
            while let Ok(Some(chunk)) = inner.next() {
                if chunk.header().tag == tag {
                    chunk
                        .check_body_checksum(&mut scratch)
                        .map_err(|_| LocalVpdError::InvalidChecksum)?;

                    if chunk.len() as usize != core::mem::size_of::<V>() {
                        return Err(LocalVpdError::InvalidChunkSize);
                    }

                    let mut out = V::new_zeroed();
                    chunk
                        .read_exact(0, out.as_bytes_mut())
                        .map_err(|_| LocalVpdError::DeviceError)?;
                    return Ok(out);
                }
            }
            return Err(LocalVpdError::NoSuchChunk);
        }
    }
    Err(LocalVpdError::NoRootChunk)
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
