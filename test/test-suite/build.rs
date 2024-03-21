// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_m_profile()?;

    #[cfg(feature = "i2c-devices")]
    build_i2c::codegen(build_i2c::Disposition::Devices)?;

    build_util::build_notifications()?;

    Ok(())
}
