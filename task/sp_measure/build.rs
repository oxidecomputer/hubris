// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::Write;

const TEST_SIZE: usize = 0x0010_0000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("expected.rs");
    let mut file = std::fs::File::create(&dest_path)?;

    writeln!(&mut file, "const FLASH_START: u32 = 0x0800_0000;").unwrap();
    writeln!(&mut file, "const TEST_SIZE: u32 = {};", TEST_SIZE).unwrap();
    writeln!(&mut file, "const FLASH_END: u32 = FLASH_START + TEST_SIZE;")
        .unwrap();

    Ok(())
}
