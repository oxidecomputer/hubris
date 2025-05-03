// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Grapefruit-specific packrat data.

use task_packrat_api::HostStartupOptions;

pub(crate) struct GrapefruitData {
    host_startup_options: HostStartupOptions,
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

impl GrapefruitData {
    pub(crate) fn new() -> Self {
        Self {
            host_startup_options: default_host_startup_options(),
        }
    }

    pub(crate) fn host_startup_options(&self) -> HostStartupOptions {
        self.host_startup_options
    }

    pub(crate) fn set_host_startup_options(
        &mut self,
        options: HostStartupOptions,
    ) {
        self.host_startup_options = options;
    }
}
