// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Packrat data for boards which are not expected to have a host (i.e.
//! everything that isn't a compute sled or compute sled development
//! board.

use crate::bsp::Bsp;
use crate::host::HostCrashDebuggingInfo;
use crate::spd_data::NoSpdData;
use task_packrat_api::HostStartupOptions;

pub(crate) struct BspImpl {}

impl Bsp for BspImpl {
    type Spd = NoSpdData;

    fn new() -> Self {
        Self {}
    }

    fn host_startup_options(&self) -> Option<&HostStartupOptions> {
        None
    }

    fn host_startup_options_mut(&mut self) -> Option<&mut HostStartupOptions> {
        None
    }

    fn host_info(&self) -> Option<&HostCrashDebuggingInfo> {
        None
    }

    fn host_info_mut(&mut self) -> Option<&mut HostCrashDebuggingInfo> {
        None
    }

    fn spd(&self) -> Option<&NoSpdData> {
        None
    }

    fn spd_mut(&mut self) -> Option<&mut NoSpdData> {
        None
    }
}
