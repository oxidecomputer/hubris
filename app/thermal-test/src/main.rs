// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The kernel for the `thermal-test` simulation, built for the host. See
//! `app.toml` next to this file for what runs under it.

fn main() -> ! {
    unsafe { kern::startup::start_kernel(0) }
}
