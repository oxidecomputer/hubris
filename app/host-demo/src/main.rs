// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The kernel, built for the host machine.
//!
//! This is the same kernel that runs on the SP, with its `host` architecture
//! selected. Instead of a vector table and an MPU it gets a set of task
//! processes to launch and talk to; see `kern::arch::host` for how it finds
//! them. Build and run it with `cargo xtask host-run app/host-demo/app.toml`.

fn main() -> ! {
    // There is no hardware tick to divide; time is virtual on the host.
    unsafe { kern::startup::start_kernel(0) }
}
