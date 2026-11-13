// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() {
    // robo: The I2C device table comes from the manifest, as it does for
    // robo: the task under test.
    if let Err(e) = build_i2c::codegen_to_file(build_i2c::Disposition::Devices)
    {
        println!("cargo::error=code generation failed: {e}");
        std::process::exit(1);
    }
}
