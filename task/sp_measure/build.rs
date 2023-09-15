// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use sha3::{Digest, Sha3_256};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct TaskConfig {
    binary_path: PathBuf,
}

const TEST_SIZE: usize = 0x0010_0000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("expected.rs");
    let mut file = std::fs::File::create(dest_path)?;

    let task_config = build_util::task_config::<TaskConfig>()?;

    println!("cargo:rerun-if-changed={:?}", task_config.binary_path);

    // We intentionally don't error out of the binary path isn't
    // found. There's no way to have another binary available for CI
    // unless we check something in which will still be wrong. It's
    // still useful to calculate a hash to demonstrate the connection
    // works.
    let bin = match std::fs::read(&task_config.binary_path) {
        Ok(b) => b,
        Err(_) => vec![0; 256],
    };

    writeln!(&mut file, "const FLASH_START: u32 = 0x0800_0000;").unwrap();
    writeln!(&mut file, "const TEST_SIZE: u32 = {};", TEST_SIZE).unwrap();
    writeln!(&mut file, "const FLASH_END: u32 = FLASH_START + TEST_SIZE;")
        .unwrap();

    let mut sha = Sha3_256::new();
    sha.update(&bin);

    let extra: Vec<u8> = vec![0xff; TEST_SIZE - bin.len()];

    sha.update(&extra);

    let sha_out = sha.finalize();

    writeln!(&mut file, "const EXPECTED : [u8; 32] = [").unwrap();
    for b in sha_out {
        writeln!(&mut file, "0x{:x},", b).unwrap();
    }
    writeln!(&mut file, "];").unwrap();

    Ok(())
}
