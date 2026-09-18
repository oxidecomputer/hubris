// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // On a native (host) target this sets no cfgs; the crate then builds
    // its host syscall implementation, for running tasks under a fixture.
    build_util::expose_m_profile()?;
    Ok(())
}
