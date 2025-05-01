// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

//! Driver to read vital product data (VPD) from the local FRU ID EEPROM.
//!
//! `read_config` reads from the *local* EEPROM; i.e. is the one soldered to the
//! PCB itself.  The app TOML file must have one AT24xx named `local_vpd`; we
//! use that name to pick which EEPROM to read in `read_config`.

use drv_i2c_devices::at24csw080::At24Csw080;
use drv_oxide_vpd::VpdError;
use userlib::TaskId;
use zerocopy::{FromBytes, IntoBytes};

pub use drv_oxide_vpd::VpdError as LocalVpdError;

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
pub fn read_config<V: IntoBytes + FromBytes>(
    i2c_task: TaskId,
    tag: [u8; 4],
) -> Result<V, VpdError> {
    let eeprom =
        At24Csw080::new(i2c_config::devices::at24csw080_local_vpd(i2c_task));
    drv_oxide_vpd::read_config_from(eeprom, tag)
}

/// Searches for the given TLV-C tag in the local VPD and reads it
///
/// Calls into [`drv_oxide_vpd::read_config_from_into`]; see details in that
/// docstring
pub fn read_config_into(
    i2c_task: TaskId,
    tag: [u8; 4],
    out: &mut [u8],
) -> Result<usize, VpdError> {
    let eeprom =
        At24Csw080::new(i2c_config::devices::at24csw080_local_vpd(i2c_task));
    drv_oxide_vpd::read_config_from_into(eeprom, tag, out)
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
