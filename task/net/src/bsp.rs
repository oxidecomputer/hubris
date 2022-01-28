// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// These modules are exported so that we don't have warnings about unused code,
// but you should import Bsp instead, which is autoselected based on board.

cfg_if::cfg_if! {
    if #[cfg(any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"))] {
        pub mod nucleo_h7;
        pub use nucleo_h7::Bsp as Bsp;
    } else if #[cfg(target_board = "sidecar-1")] {
        pub mod sidecar_1;
        pub use sidecar_1::Bsp as Bsp;
    } else if #[cfg(target_board = "gimletlet-1")] {
        pub mod gimletlet_mgmt;
        pub use gimletlet_mgmt::Bsp as Bsp;
    } else {
        compile_error!("Board is not supported by the task/net");
    }
}
