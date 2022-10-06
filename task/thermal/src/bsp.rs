// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

cfg_if::cfg_if! {
    if #[cfg(target_board = "gimlet-b")] {
        mod gimlet_b;
        pub(crate) use gimlet_b::*;
    } else if #[cfg(target_board = "sidecar-a")] {
        mod sidecar_a;
        pub(crate) use sidecar_a::*;
    } else {
        compile_error!("No BSP for the given board");
    }
}
