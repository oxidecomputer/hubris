// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Grapefruit-specific packrat data.

use crate::bsp::Bsp;
use crate::host::HostCrashDebuggingInfo;
use crate::spd_data::NoSpdData;
use static_cell::ClaimOnceCell;
use task_packrat_api::HostStartupOptions;

pub(crate) struct BspImpl {
    host_startup_options: HostStartupOptions,
    host_info: &'static mut HostCrashDebuggingInfo,
}

const fn default_host_startup_options() -> HostStartupOptions {
    if cfg!(feature = "boot-kmdb") {
        // We have to do this because const fn.
        let bits = HostStartupOptions::STARTUP_KMDB.bits()
            | HostStartupOptions::STARTUP_PROM.bits()
            | HostStartupOptions::STARTUP_VERBOSE.bits()
            | HostStartupOptions::STARTUP_BOOT_RAMDISK.bits();
        match HostStartupOptions::from_bits(bits) {
            Some(options) => options,
            None => panic!("must be valid at compile-time"),
        }
    } else {
        HostStartupOptions::empty()
    }
}

impl Bsp for BspImpl {
    // Grapefruit has a host, but we don't support SPD data for it.
    type Spd = NoSpdData;

    fn new() -> Self {
        static HOST_INFO: ClaimOnceCell<HostCrashDebuggingInfo> =
            ClaimOnceCell::new(HostCrashDebuggingInfo::new());
        Self {
            host_startup_options: default_host_startup_options(),
            host_info: HOST_INFO.claim(),
        }
    }

    fn host_startup_options(&self) -> Option<&HostStartupOptions> {
        Some(&self.host_startup_options)
    }

    fn host_startup_options_mut(&mut self) -> Option<&mut HostStartupOptions> {
        Some(&mut self.host_startup_options)
    }

    fn host_info(&self) -> Option<&HostCrashDebuggingInfo> {
        Some(self.host_info)
    }

    fn host_info_mut(&mut self) -> Option<&mut HostCrashDebuggingInfo> {
        Some(self.host_info)
    }

    fn spd(&self) -> Option<&NoSpdData> {
        None
    }

    fn spd_mut(&mut self) -> Option<&mut NoSpdData> {
        None
    }
}
