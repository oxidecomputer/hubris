// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde_json::Value;
use std::fmt::Write;
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_target_board();

    let fpga_image = fs::read("fpga.bin")?;
    let compressed = compress(&fpga_image);

    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("fpga.bin.rle"), compressed)?;

    let disposition = build_i2c::Disposition::Devices;

    #[cfg(feature = "standalone")]
    let artifact = build_i2c::Artifact::Standalone;

    #[cfg(not(feature = "standalone"))]
    let artifact = build_i2c::Artifact::Dist;

    if let Err(e) = build_i2c::codegen(disposition, artifact) {
        println!("code generation failed: {}", e);
        std::process::exit(1);
    }

    fs::write(out.join("gimlet_regs.rs"), regs()?)?;
    println!("cargo:rerun-if-changed=fpga.bin");

    idol::server::build_server_support(
        "../../idl/gimlet-seq.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    Ok(())
}

fn regs() -> Result<String, Box<dyn std::error::Error>> {
    let mut output = String::new();
    let regs = include_str!("gimlet_regs.json");
    let v: Value = serde_json::from_str(regs)?;

    let c = &v["children"];

    writeln!(
        &mut output,
        r##"
#[allow(non_camel_case_types)]
pub enum Addr {{"##
    )?;

    if let Value::Array(arr) = c {
        for child in arr {
            if let Value::String(str) = &child["type"] {
                if str == "reg" {
                    let name = match &child["inst_name"] {
                        Value::String(str) => str,
                        _ => panic!("malformed regsister name: {:#?}", child),
                    };

                    let offset = match &child["addr_offset"] {
                        Value::Number(offset) => match offset.as_u64() {
                            Some(offset) => offset,
                            None => {
                                panic!("malformed offset: {:#?}", child)
                            }
                        },
                        _ => panic!("malformed register: {:#?}", child),
                    };

                    writeln!(&mut output, "    {} = 0x{:x},", name, offset)?;
                }
            }
        }
    }

    println!("cargo:rerun-if-changed=gimlet_regs.json");
    writeln!(&mut output, "}}")?;

    writeln!(
        &mut output,
        r##"
impl From<Addr> for u16 {{
    fn from(a: Addr) -> Self {{
        a as u16
    }}
}}"##
    )?;

    Ok(output)
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
