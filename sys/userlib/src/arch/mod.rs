// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

cfg_if::cfg_if! {
    if #[cfg(target_os = "none")] {
        mod thumb;
        pub use thumb::*;
    } else {
        compile_error!("Unsupported!");
    }
}
