// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_target_board();

    let fpga_image = fs::read("fpga.bin")?;
    let compressed = compress(&fpga_image);

    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("fpga.bin.rle"), compressed)?;
    Ok(())
}

fn compress(input: &[u8]) -> Vec<u8> {
    let mut output = vec![];
    gnarle::compress(input, |chunk| {
        output.extend_from_slice(chunk);
        Ok::<_, std::convert::Infallible>(())
    })
    .ok();
    output
}
