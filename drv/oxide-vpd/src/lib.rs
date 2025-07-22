// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

//! Driver to read vital product data (VPD) from a FRU ID EEPROM.
//!
//! We assume that the all EEPROMs are AT24CSW080s, and that they contains keys
//! in TLV-C format (see RFD 148 for a general description, or RFD 320 for the
//! specific example of MAC addresses)

use drv_i2c_devices::at24csw080::{
    At24Csw080, Error as At24Error, EEPROM_SIZE,
};
use ringbuf::*;
use tlvc::{ChunkHandle, TlvcRead, TlvcReadError, TlvcReader};
use zerocopy::{FromBytes, IntoBytes};

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
    None,
    EepromError(drv_i2c_devices::at24csw080::Error),
    Error(VpdError),
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
/// `read_config_from` should be called with a tag nested under `FRU0` (e.g.
/// `TAG1` in the example above).  It will deserialize the raw byte array (shown
/// as `[...]`) into an object of type `V`.
pub fn read_config_from<V: IntoBytes + FromBytes>(
    eeprom: At24Csw080,
    tag: [u8; 4],
) -> Result<V, VpdError> {
    read_config_nested_from(eeprom, &[(tag, 0)])
}

/// Searches for a TLV-C tag which may be nested and/or repeated.
///
/// For example, consider EEPROM data of the form
/// ```ron
/// ("FRU0", [
///     ("TAG1", [
///         ("TAG2", [ [...] ]),
///         ("TAG2", [ [...] ]),
///         ("TAG2", [ [...] ]),
///     ]),
/// ])
/// ```
///
/// To get the first `TAG2`, this would be called with
/// `tags = &[(*b"TAG1", 0), (*b"TAG2", 0)]`.
///
/// To get the second `TAG2`, `&[(*b"TAG1", 0), (*b"TAG2", 1)]`, and so on.
///
/// The `FRU0` root is mandatory, but not included in the `tags` argument.
pub fn read_config_nested_from<V: IntoBytes + FromBytes>(
    eeprom: At24Csw080,
    tags: &[([u8; 4], usize)],
) -> Result<V, VpdError> {
    let mut out = V::new_zeroed();
    let n = read_config_nested_from_into(eeprom, tags, out.as_mut_bytes())?;

    // `read_config_nested_from_into(..)` fails if the data is too large for
    // `out`, but will succeed if it's less than out; we want to guarantee it's
    // exactly the size of V.
    if n != core::mem::size_of::<V>() {
        return Err(VpdError::InvalidChunkSize);
    }

    Ok(out)
}

/// Searches for the given TLV-C tag in the given VPD and reads it
///
/// See [`read_config_from`] docs for details on EEPROM format
pub fn read_config_from_into(
    eeprom: At24Csw080,
    tag: [u8; 4],
    out: &mut [u8],
) -> Result<usize, VpdError> {
    read_config_nested_from_into(eeprom, &[(tag, 0)], out)
}

/// Searches for a TLV-C tag which may be nested or repeated
///
/// See [`read_config_nested_from`] for docs on how this can be used
pub fn read_config_nested_from_into(
    eeprom: At24Csw080,
    tag: &[([u8; 4], usize)],
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

/// Inner function, without logging
///
/// Any error returned from this routine will be put into a ringbuf by its
/// caller, so we can return them easily with `?`.
fn read_config_inner(
    eeprom: At24Csw080,
    tags: &[([u8; 4], usize)],
    out: &mut [u8],
) -> Result<usize, VpdError> {
    let eeprom_reader = EepromReader { eeprom: &eeprom };
    let reader =
        TlvcReader::begin(eeprom_reader).map_err(VpdError::ErrorOnBegin)?;

    // Find the root chunk, translating from a general to specific error
    let mut chunk =
        get_chunk_for_tag(reader, *b"FRU0", 0).map_err(|e| match e {
            VpdError::NoSuchChunk(..) => VpdError::NoRootChunk,
            e => e,
        })?;

    // Iterate over our tag list, finding inner chunks
    for &(tag, index) in tags {
        let inner = chunk.read_as_chunks();
        chunk = get_chunk_for_tag(inner, tag, index)?;
    }

    // Deserialize the found chunk
    let chunk_len = chunk.len() as usize;
    if chunk_len > out.len() {
        return Err(VpdError::InvalidChunkSize);
    }

    chunk
        .read_exact(0, &mut out[..chunk_len])
        .map_err(VpdError::ErrorOnRead)?;
    Ok(chunk_len)
}

/// Searches for a single tag, which may appear multiple times
///
/// Returns the `index`'th chunk with a matching tag.
fn get_chunk_for_tag(
    mut reader: TlvcReader<EepromReader<'_>>,
    tag: [u8; 4],
    index: usize,
) -> Result<ChunkHandle<EepromReader<'_>>, VpdError> {
    let mut count = 0;
    loop {
        match reader.next() {
            Ok(Some(chunk)) => {
                let mut scratch = [0u8; 32];
                if chunk.header().tag == tag {
                    if count == index {
                        chunk
                            .check_body_checksum(&mut scratch)
                            .map_err(VpdError::InvalidChecksum)?;
                        break Ok(chunk);
                    }
                    count += 1;
                }
            }
            Ok(None) => return Err(VpdError::NoSuchChunk(tag)),
            Err(e) => return Err(VpdError::ErrorOnNext(e)),
        }
    }
}
