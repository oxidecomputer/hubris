// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common implementation for device inventory
//!
//! Board-specific inventory is implemented in the `bsp` subfolder and imported
//! as the `bsp` module.

/// Inventory API version (always 0 for now)
pub(crate) const INVENTORY_API_VERSION: u32 = 0;
