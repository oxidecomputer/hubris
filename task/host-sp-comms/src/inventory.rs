// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common implementation for device inventory
//!
//! Board-specific inventory is implemented in the `bsp` subfolder and imported
//! as the `bsp` module.

/// Inventory API version (always 0 for now)
pub(crate) const INVENTORY_API_VERSION: u32 = 0;

#[allow(unused_macros)]
macro_rules! by_refdes {
    ($refdes:ident, $dev:ident) => {
        paste::paste! {{
            (
                i2c_config::devices::[<$dev _ $refdes:lower >](I2C.get_task_id()),
                i2c_config::sensors::[<$dev:upper _ $refdes:upper _SENSORS>]
            )
        }}
    };
}

#[allow(unused_imports)]
pub(crate) use by_refdes;

#[cfg(any(
    target_board = "gimlet-b",
    target_board = "gimlet-c",
    target_board = "gimlet-d",
    target_board = "gimlet-e",
    target_board = "gimlet-f",
    target_board = "cosmo-a",
    target_board = "cosmo-b",
))]
pub(crate) mod compute_sled_common;
