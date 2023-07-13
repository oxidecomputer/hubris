// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

//! Driver to read vital product data (VPD) from a local FRU ID EEPROM.
//!
//! `read_config` reads from the *local* EEPROM; i.e. is the one soldered to the
//! PCB itself.  The app TOML file must have one AT24xx named `local_vpd`; we
//! use that name to pick which EEPROM to read in `read_config`.
//!
//! The system may have additional EEPROMs on FRUs that plug into the board
//! (e.g. fans); those can be read with `read_config_from`. We assume that the
//! all EEPROMs are AT24CSW080s, and that they contains keys in TLV-C format
//! (see RFD 148 for a general description, or RFD 320 for the specific example
//! of MAC addresses)

use drv_i2c_devices::at24csw080::{
    At24Csw080, Error as At24Error, EEPROM_SIZE,
};
use ringbuf::*;
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use zerocopy::{AsBytes, FromBytes};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VpdError {
    ErrorOnBegin(TlvcReadError<At24Error>),
    ErrorOnRead(TlvcReadError<At24Error>),
    ErrorOnNext(TlvcReadError<At24Error>),
    NoSuchChunk([u8; 4]),
    InvalidChecksum(TlvcReadError<At24Error>),
    InvalidChunkSize,
    /// The base FRU0 chunk we expected was not present.
    NoRootChunk,
}

#[derive(Clone)]
struct EepromReader<'a> {
    eeprom: &'a At24Csw080,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    EepromError(drv_i2c_devices::at24csw080::Error),
    Error(VpdError),
    UnrelatedChunk([u8; 4]),
    None,
}

ringbuf!(Trace, 4, Trace::None);

impl<'a> TlvcRead for EepromReader<'a> {
    type Error = drv_i2c_devices::at24csw080::Error;
    fn extent(&self) -> Result<u64, TlvcReadError<Self::Error>> {
        Ok(EEPROM_SIZE as u64)
    }
    fn read_exact(
        &self,
        offset: u64,
        dest: &mut [u8],
    ) -> Result<(), TlvcReadError<Self::Error>> {
        self.eeprom.read_into(offset as u16, dest).map_err(|code| {
            ringbuf_entry!(Trace::EepromError(code));
            TlvcReadError::User(code)
        })?;
        Ok(())
    }
}

/// Searches for the given TLV-C tag in the given VPD and reads it
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
pub fn read_config_from<V: AsBytes + FromBytes>(
    eeprom: At24Csw080,
    tag: [u8; 4],
) -> Result<V, VpdError> {
    let mut out = V::new_zeroed();
    let n = read_config_from_into(eeprom, tag, out.as_bytes_mut())?;

    // `read_config_into()` fails if the data is too large for `out`, but will
    // succeed if it's less than out; we want to guarantee it's exactly the size
    // of V.
    if n != core::mem::size_of::<V>() {
        return Err(VpdError::InvalidChunkSize);
    }

    Ok(out)
}

/// Searches for the given TLV-C tag in the given VPD and reads it
///
/// Returns an error if the tag is not present, the data is too large to fit in
/// `out`, or any checksum is corrupt.
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
/// in the example above).  It will copy the raw byte array (shown as
/// `[...]`) into `out`, returning the number of bytes written.
pub fn read_config_from_into(
    eeprom: At24Csw080,
    tag: [u8; 4],
    out: &mut [u8],
) -> Result<usize, VpdError> {
    match read_config_inner(eeprom, tag, out) {
        Ok(n) => Ok(n),
        Err(e) => {
            ringbuf_entry!(Trace::Error(e));
            Err(e)
        }
    }
}

/// Implementation factor of `read_config_into` above to ensure that all errors
/// are recorded. Any error returned from this routine will be put into a
/// ringbuf by its caller, so it needn't worry about it.
fn read_config_inner(
    eeprom: At24Csw080,
    tag: [u8; 4],
    out: &mut [u8],
) -> Result<usize, VpdError> {
    let eeprom_reader = EepromReader { eeprom: &eeprom };

    let mut reader =
        TlvcReader::begin(eeprom_reader).map_err(VpdError::ErrorOnBegin)?;

    loop {
        match reader.next() {
            Ok(Some(chunk)) => {
                let mut scratch = [0u8; 32];
                if chunk.header().tag == *b"FRU0" {
                    chunk
                        .check_body_checksum(&mut scratch)
                        .map_err(VpdError::InvalidChecksum)?;
                    let mut inner = chunk.read_as_chunks();
                    while let Ok(Some(chunk)) = inner.next() {
                        if chunk.header().tag == tag {
                            chunk
                                .check_body_checksum(&mut scratch)
                                .map_err(VpdError::InvalidChecksum)?;

                            let chunk_len = chunk.len() as usize;

                            if chunk_len > out.len() {
                                return Err(VpdError::InvalidChunkSize);
                            }

                            chunk
                                .read_exact(0, &mut out[..chunk_len])
                                .map_err(VpdError::ErrorOnRead)?;
                            return Ok(chunk_len);
                        }
                    }
                    return Err(VpdError::NoSuchChunk(tag));
                } else {
                    ringbuf_entry!(Trace::UnrelatedChunk(chunk.header().tag));
                }
            }
            Ok(None) => return Err(VpdError::NoRootChunk),
            Err(e) => return Err(VpdError::ErrorOnNext(e)),
        }
    }
}
