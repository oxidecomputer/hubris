// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common code for reading board VPD and populating `packrat`.

#![no_std]

use drv_local_vpd::LocalVpdError;
use oxide_barcode::ParseError as BarcodeParseError;
use ringbuf::{ringbuf, ringbuf_entry};
use task_packrat_api::{CacheSetError, MacAddressBlock, OxideIdentity};
use userlib::{hl, TaskId};

pub use task_packrat_api::Packrat;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    MacLocalVpdError(LocalVpdError),
    BarcodeLocalVpdError(LocalVpdError),
    BarcodeParseError(BarcodeParseError),
    MacsAlreadySet(MacAddressBlock),
    IdentityAlreadySet(OxideIdentity),
}

ringbuf!(Trace, 16, Trace::None);

pub fn read_vpd_and_load_packrat(packrat: &Packrat, i2c_task: TaskId) {
    // How many times are we willing to try reading VPD if it fails, and how
    // long do we wait between retries?
    const MAX_ATTEMPTS: usize = 5;
    const SLEEP_BETWEEN_RETRIES_MS: u64 = 500;

    let mut read_macs = false;
    let mut read_identity = false;

    for _ in 0..MAX_ATTEMPTS {
        if !read_macs {
            match drv_local_vpd::read_config(i2c_task, *b"MAC0") {
                Ok(macs) => {
                    match packrat.set_mac_address_block(macs) {
                        Ok(()) => (),
                        Err(CacheSetError::ValueAlreadySet) => {
                            ringbuf_entry!(Trace::MacsAlreadySet(macs));
                        }
                    }
                    read_macs = true;
                }
                Err(err) => {
                    ringbuf_entry!(Trace::MacLocalVpdError(err));
                }
            }
        }

        if !read_identity {
            match read_oxide_barcode(i2c_task) {
                Ok(identity) => {
                    match packrat.set_identity(identity) {
                        Ok(()) => (),
                        Err(CacheSetError::ValueAlreadySet) => {
                            ringbuf_entry!(Trace::IdentityAlreadySet(identity));
                        }
                    }
                    read_identity = true;
                }
                Err(VpdIdentityError::LocalVpdError(err)) => {
                    ringbuf_entry!(Trace::BarcodeLocalVpdError(err));
                }
                Err(VpdIdentityError::ParseError(err)) => {
                    ringbuf_entry!(Trace::BarcodeParseError(err));
                }
            }
        }

        if read_macs && read_identity {
            break;
        }

        hl::sleep_for(SLEEP_BETWEEN_RETRIES_MS);
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum VpdIdentityError {
    LocalVpdError(LocalVpdError),
    ParseError(BarcodeParseError),
}

impl From<LocalVpdError> for VpdIdentityError {
    fn from(err: LocalVpdError) -> Self {
        Self::LocalVpdError(err)
    }
}

impl From<BarcodeParseError> for VpdIdentityError {
    fn from(err: BarcodeParseError) -> Self {
        Self::ParseError(err)
    }
}

/// Read the Oxide barcode tag and parse it.
///
/// Supports `0XV1` and `0XV2` formats.
fn read_oxide_barcode(
    i2c_task: TaskId,
) -> Result<OxideIdentity, VpdIdentityError> {
    // 0XV1 barcodes are 31 bytes and 0XV2 barcodes are 32 bytes; those are
    // the only two version we know how to parse today, so we're safe with a
    // 32-byte output buffer.
    let mut barcode = [0; 32];

    let n = drv_local_vpd::read_config_into(i2c_task, *b"BARC", &mut barcode)?;
    let identity = OxideIdentity::parse(&barcode[..n])?;

    Ok(identity)
}
