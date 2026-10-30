// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#[cfg(target_os = "none")]
mod arm_m;
#[cfg(not(target_os = "none"))]
mod host;

#[cfg(target_os = "none")]
pub use arm_m::{TaskSlot, TaskSlotTableEntry};
#[cfg(not(target_os = "none"))]
pub use host::TaskSlot;
