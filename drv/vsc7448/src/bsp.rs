// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// These modules are exported so that we don't have warnings about unused code,
// but you should import Bsp instead, which is autoselected based on board.
pub mod gemini_bu;
pub mod sidecar_1;

cfg_if::cfg_if! {
    if #[cfg(target_board = "gemini-bu-1")] {
        pub use gemini_bu::Bsp;
    } else if #[cfg(target_board = "sidecar-1")] {
        pub use sidecar_1::Bsp;
    } else {
        compile_error!("No BSP available for this board");
    }
}
