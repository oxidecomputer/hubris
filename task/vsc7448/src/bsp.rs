// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// These modules are exported so that we don't have warnings about unused code,
// but you should import Bsp instead, which is autoselected based on board.

cfg_if::cfg_if! {
    // We use the gemini_bu Bsp for both the Gemini BU and the Gimletlet host.
    // In both cases, these are driving a VSC7448 dev kit on someone's desk.
    if #[cfg(any(target_board = "gemini-bu-1",
                 target_board = "gimletlet-2"))] {
        pub mod gemini_bu;
        pub use gemini_bu::Bsp;
    } else if #[cfg(target_board = "sidecar-1")] {
        pub mod sidecar_1;
        pub use sidecar_1::Bsp;
    } else {
        compile_error!("No BSP available for this board");
    }
}
