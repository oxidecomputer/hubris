// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Target-specific syscall implementations.
//!
//! Exactly one submodule is compiled in, and it must provide the `sys_*`
//! functions, `_start`, and the panic handlers that the crate root re-exports.
//! Everything shared between implementations (argument types, results,
//! convenience wrappers) lives in the crate root instead.

cfg_if::cfg_if! {
    if #[cfg(target_os = "none")] {
        mod thumb;
        pub use thumb::*;
    } else {
        mod host;
        pub use host::*;
    }
}
