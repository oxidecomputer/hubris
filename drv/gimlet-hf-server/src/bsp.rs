// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// We deliberately build every possible BSP here; the linker will strip them,
// and this prevents us from accidentally introducing breaking changes.
mod gemini_bu_1;
mod gimlet_b;
mod gimletlet_2;
mod nucleo_h7x;

cfg_if::cfg_if! {
    if #[cfg(target_board = "gimlet-b")] {
        pub(crate) use gimlet_b::*;
    } else if #[cfg(target_board = "gemini-bu-1")] {
        pub(crate) use gemini_bu_1::*;
    } else if #[cfg(target_board = "gimletlet-2")] {
        pub(crate) use gimletlet_2::*;
    } else if #[cfg(any(target_board = "nucleo-h743zi2",
                        target_board = "nucleo-h753zi"))] {
        pub(crate) use nucleo_h7x::*;
    }
}
