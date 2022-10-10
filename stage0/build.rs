// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut const_file = File::create(out.join("consts.rs")).unwrap();

    println!("cargo:rerun-if-env-changed=HUBRIS_IMAGE_ID");
    let image_id: u64 = env::var("HUBRIS_IMAGE_ID")?.parse()?;

    writeln!(const_file, "// See build.rs for details")?;

    writeln!(const_file, "#[used]")?;
    writeln!(const_file, "#[no_mangle]")?;
    writeln!(const_file, "#[link_section = \".hubris_id\"]")?;
    writeln!(
        const_file,
        "pub static HUBRIS_IMAGE_ID: u64 = {};",
        image_id
    )?;

    Ok(())
}
