// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// These modules are exported so that we don't have warnings about unused code,
// but you should import Bsp instead, which is autoselected based on board.

cfg_if::cfg_if! {
    // We use the vsc7448_dev Bsp for both Gemini-BU and Gimletlet hosts.
    // In both cases, these are driving a VSC7448 dev kit on someone's desk,
    // connected over wires to the Hubris board.
    if #[cfg(any(target_board = "gemini-bu-1",
                 target_board = "gimletlet-2"))] {
        pub mod vsc7448_dev;
        pub use vsc7448_dev::Bsp;
    } else if #[cfg(target_board = "sidecar-1")] {
        pub mod sidecar_1;
        pub use sidecar_1::Bsp;
    } else {
        compile_error!("No BSP available for this board");
    }
}
