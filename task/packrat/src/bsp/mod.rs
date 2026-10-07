// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::host::HostCrashDebuggingInfo;
use crate::spd_data::SpdStore;
use task_packrat_api::HostStartupOptions;

// Select an impl and re-export based on the board family features
cfg_if::cfg_if! {
    if #[cfg(feature = "gimlet")] {
        mod gimlet;
        pub(crate) use gimlet::BspImpl;
    } else if #[cfg(any(feature = "cosmo", feature = "metro"))] {
        mod cosmo_metro;
        pub(crate) use cosmo_metro::BspImpl;
    } else if #[cfg(feature = "grapefruit")] {
        mod grapefruit;
        pub(crate) use grapefruit::BspImpl;
    } else {
        // Everything else is a board that is not expected to have a host.
        mod no_host;
        pub(crate) use no_host::BspImpl;
    }
}

/// Board-specific customization of packrat.
///
/// Each board (or family of boards) is expected to provide a `BspImpl` type
/// that implements this trait.
pub(crate) trait Bsp {
    /// Storage for cached SPD data.
    ///
    /// BSPs which do not cache SPD data should use
    /// [`NoSpdData`](crate::spd_data::NoSpdData) and return `None` from
    /// [`Bsp::spd`] and [`Bsp::spd_mut`].
    type Spd: SpdStore;

    /// Constructs the BSP, claiming any static storage it requires.
    ///
    /// This may only be called once.
    fn new() -> Self;

    /// Startup options to be given to the host the next time it boots, or
    /// `None` if this board is not expected to have a host.
    fn host_startup_options(&self) -> Option<&HostStartupOptions>;
    /// Same as [`Self::host_startup_options`], but returns mutable access.
    fn host_startup_options_mut(&mut self) -> Option<&mut HostStartupOptions>;

    /// Crash debugging information recorded from the host, or `None` if this
    /// board is not expected to have a host.
    fn host_info(&self) -> Option<&HostCrashDebuggingInfo>;
    /// Same as [`Self::host_info`], but returns mutable access.
    fn host_info_mut(&mut self) -> Option<&mut HostCrashDebuggingInfo>;

    /// Cached SPD data, or `None` if this board does not cache SPD data.
    fn spd(&self) -> Option<&Self::Spd>;
    /// Same as [`Self::spd`], but returns mutable access.
    fn spd_mut(&mut self) -> Option<&mut Self::Spd>;
}
