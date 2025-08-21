// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common implementation for device inventory
//!
//! Board-specific inventory is implemented in the `bsp` subfolder and imported
//! as the `bsp` module.

/// Inventory API version (always 0 for now)
pub(crate) const INVENTORY_API_VERSION: u32 = 0;

/// `const` function to convert a `&'static str` to a fixed-size byte array
///
/// This must be called a `const` parameter of `s.len()`
#[allow(dead_code)]
pub(crate) const fn byteify<const N: usize>(s: &'static [u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < s.len() {
        out[i] = if s[i] == b'_' { b'/' } else { s[i] };
        i += 1;
    }
    out
}

#[allow(unused_macros)]
macro_rules! by_refdes {
    // Length is found based on refdes
    ($refdes:ident, $dev:ident) => {
        by_refdes!($refdes, $dev, stringify!($refdes).as_bytes().len())
    };
    // Length is provided, e.g. if it is not consistent between refdes
    ($refdes:ident, $dev:ident, $n:expr) => {
        paste::paste! {{
            const BYTE_ARRAY: &'static [u8] = stringify!($refdes).as_bytes();
            (
                $crate::inventory::byteify::<{ $n }>(BYTE_ARRAY),
                i2c_config::devices::[<$dev _ $refdes:lower >] as fn(TaskId) -> I2cDevice,
                i2c_config::sensors::[<$dev:upper _ $refdes:upper _SENSORS>]
            )
        }}
    };
}

#[allow(unused_imports)]
pub(crate) use by_refdes;
