// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::host::HostCrashDebuggingInfo;
use crate::spd_data::SpdStore;
use task_packrat_api::HostStartupOptions;

// Select an impl and re-export based on the target board
//
// This is intentionally verbose, to ensure that when a new board is added we
// emit a compilation error until it is added here.
cfg_if::cfg_if! {
    if #[cfg(any(
        target_board = "gimlet-b",
        target_board = "gimlet-c",
        target_board = "gimlet-d",
        target_board = "gimlet-e",
        target_board = "gimlet-f",
    ))] {
        mod gimlet;
        pub(crate) use gimlet::BspImpl;
    } else if #[cfg(any(
        target_board = "cosmo-a",
        target_board = "cosmo-b",
        target_board = "metro-a",
    ))] {
        mod cosmo_metro;
        pub(crate) use cosmo_metro::BspImpl;
    } else if #[cfg(any(
        target_board = "grapefruit-a",
        target_board = "grapefruit-b",
    ))] {
        mod grapefruit;
        pub(crate) use grapefruit::BspImpl;
    } else if #[cfg(any(
        target_board = "gimletlet-2",
        target_board = "medusa-a",
        target_board = "minibar-a",
        target_board = "minibar-b",
        target_board = "nucleo-h743zi2",
        target_board = "nucleo-h753zi",
        target_board = "observer-a",
        target_board = "psc-b",
        target_board = "psc-c",
        target_board = "sidecar-b",
        target_board = "sidecar-c",
        target_board = "sidecar-d",
    ))] {
        // Boards which are not expected to have a host.
        mod no_host;
        pub(crate) use no_host::BspImpl;
    } else {
        // Every board must be listed explicitly above, so that a new board
        // with a host can't silently end up with the `no_host` BSP.
        compile_error!("no packrat BSP for the given target board");
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
