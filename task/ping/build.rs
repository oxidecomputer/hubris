// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fs::File;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_m_profile();

    generate_consts()?;

    Ok(())
}

fn generate_consts() -> Result<(), Box<dyn std::error::Error>> {
    let out = build_util::out_dir();
    let mut const_file = File::create(out.join("consts.rs")).unwrap();

    // If hubris is non-secure (i.e. TZ is enabled) we need to use a
    // different address for our bad address testing since NULL will
    // trigger a secure fault
    if let Ok(secure) = build_util::env_var("HUBRIS_SECURE") {
        if secure == "0" {
            // This corresponds to USB SRAM on the LPC55
            writeln!(const_file, "pub const BAD_ADDRESS : u32 = 0x40100000;")
                .unwrap();
        } else {
            writeln!(const_file, "pub const BAD_ADDRESS : u32 = 0x0;").unwrap();
        }
    } else {
        writeln!(const_file, "pub const BAD_ADDRESS : u32 = 0x0;").unwrap();
    }

    Ok(())
}
