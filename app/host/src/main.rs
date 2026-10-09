// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The kernel, built for the host machine.

fn main() -> ! {
    // There is no hardware tick to divide; time is only an illusion.
    unsafe { kern::startup::start_kernel(0) }
}
