// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Gimlet-specific packrat data.

use task_packrat_api::HostStartupOptions;

/// SPD data for Gimlet
pub type SpdData = crate::spd_data::SpdData<16, 512>;

pub(crate) struct GimletData {
    host_startup_options: &'static mut HostStartupOptions,
    spd_data: &'static mut SpdData,
}

const fn default_host_startup_options() -> HostStartupOptions {
    if cfg!(feature = "boot-kmdb") {
        // We have to do this because const fn.
        let bits = HostStartupOptions::STARTUP_KMDB.bits()
            | HostStartupOptions::STARTUP_PROM.bits()
            | HostStartupOptions::STARTUP_VERBOSE.bits();
        match HostStartupOptions::from_bits(bits) {
            Some(options) => options,
            None => panic!("must be valid at compile-time"),
        }
    } else {
        HostStartupOptions::empty()
    }
}

pub(crate) struct StaticBufs {
    host_startup_options: HostStartupOptions,
    spd_data: SpdData,
}

impl StaticBufs {
    pub(crate) const fn new() -> Self {
        Self {
            spd_data: SpdData::new(),
            host_startup_options: default_host_startup_options(),
        }
    }
}

impl GimletData {
    pub(crate) fn new(
        StaticBufs {
            ref mut host_startup_options,
            ref mut spd_data,
        }: &'static mut StaticBufs,
    ) -> Self {
        Self {
            host_startup_options,
            spd_data,
        }
    }

    pub(crate) fn host_startup_options(&self) -> HostStartupOptions {
        *self.host_startup_options
    }

    pub(crate) fn set_host_startup_options(
        &mut self,
        options: HostStartupOptions,
    ) {
        *self.host_startup_options = options;
    }

    pub(crate) fn spd(&self) -> &SpdData {
        self.spd_data
    }

    pub(crate) fn spd_mut(&mut self) -> &mut SpdData {
        self.spd_data
    }
}
