// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Gimlet-specific packrat data.

use crate::bsp::Bsp;
use crate::host::HostCrashDebuggingInfo;
use static_cell::ClaimOnceCell;
use task_packrat_api::HostStartupOptions;

/// SPD data for Gimlet
pub(crate) type SpdData = crate::spd_data::SpdData<16, 512>;

pub(crate) struct BspImpl {
    host_startup_options: &'static mut HostStartupOptions,
    host_info: &'static mut HostCrashDebuggingInfo,
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

struct StaticBufs {
    host_startup_options: HostStartupOptions,
    host_info: HostCrashDebuggingInfo,
    spd_data: SpdData,
}

impl Bsp for BspImpl {
    type Spd = SpdData;

    fn new() -> Self {
        static BUFS: ClaimOnceCell<StaticBufs> =
            ClaimOnceCell::new(StaticBufs {
                host_startup_options: default_host_startup_options(),
                host_info: HostCrashDebuggingInfo::new(),
                spd_data: SpdData::new(),
            });
        let &mut StaticBufs {
            ref mut host_startup_options,
            ref mut host_info,
            ref mut spd_data,
        } = BUFS.claim();
        Self {
            host_startup_options,
            host_info,
            spd_data,
        }
    }

    fn host_startup_options(&self) -> Option<&HostStartupOptions> {
        Some(self.host_startup_options)
    }

    fn host_startup_options_mut(&mut self) -> Option<&mut HostStartupOptions> {
        Some(self.host_startup_options)
    }

    fn host_info(&self) -> Option<&HostCrashDebuggingInfo> {
        Some(self.host_info)
    }

    fn host_info_mut(&mut self) -> Option<&mut HostCrashDebuggingInfo> {
        Some(self.host_info)
    }

    fn spd(&self) -> Option<&SpdData> {
        Some(self.spd_data)
    }

    fn spd_mut(&mut self) -> Option<&mut SpdData> {
        Some(self.spd_data)
    }
}
