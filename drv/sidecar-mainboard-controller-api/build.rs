// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::fmt::Write;
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_target_board();

    let out_dir = &PathBuf::from(env::var_os("OUT_DIR").unwrap());

    fs::write(out_dir.join("sidecar_mainboard_controller.rs"), regs()?)?;

    let ecp5_bitstream_name = match env::var("HUBRIS_BOARD")?.as_str() {
        "gimletlet-2" => "sidecar_mainboard_emulator_ecp5_evn.bit",
        "sidecar-1" => "sidecar_mainboard_controller.bit",
        _ => {
            println!("No FPGA image for target board");
            std::process::exit(1)
        }
    };
    let fpga_bitstream = fs::read(ecp5_bitstream_name)?;
    let compressed_fpga_bitstream = compress(&fpga_bitstream);

    fs::write(out_dir.join("ecp5.bin.rle"), &compressed_fpga_bitstream)?;

    // Make sure the app image is rebuilt if the bitstream file for this target
    // changes.
    println!("cargo:rerun-if-changed={}", ecp5_bitstream_name);

    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Node {
    Addrmap {
        children: Vec<Node>,
    },
    Reg {
        inst_name: String,
        addr_offset: usize,
        regwidth: usize,
        children: Vec<Node>,
    },
    Field {
        inst_name: String,
        lsb: usize,
        msb: usize,
    },
}

fn regs() -> Result<String, Box<dyn std::error::Error>> {
    let mut output = String::new();
    let regs = include_str!("sidecar_mainboard_controller.json");

    let node: Node = serde_json::from_str(regs)?;

    let children = if let Node::Addrmap { children } = node {
        children
    } else {
        panic!("top-level node is not addrmap");
    };

    writeln!(
        &mut output,
        r##"
#[allow(non_camel_case_types)]
pub enum Addr {{"##
    )?;

    for child in children.iter() {
        if let Node::Reg {
            inst_name,
            addr_offset,
            ..
        } = child
        {
            writeln!(&mut output, "    {} = 0x{:x},", inst_name, addr_offset)?;
        } else {
            panic!("unexpected child {:?}", child);
        }
    }

    writeln!(&mut output, "}}")?;

    writeln!(
        &mut output,
        r##"
impl From<Addr> for u16 {{
    fn from(a: Addr) -> Self {{
        a as u16
    }}
}}

#[allow(non_snake_case)]
pub mod Reg {{
"##
    )?;

    for child in children.iter() {
        if let Node::Reg {
            inst_name,
            addr_offset: _,
            regwidth,
            children,
        } = child
        {
            if *regwidth != 8 {
                panic!("only 8-bit registers supported");
            }

            writeln!(
                &mut output,
                r##"
    #[allow(non_snake_case)]
    pub mod {} {{"##,
                inst_name
            )?;

            for child in children.iter() {
                if let Node::Field {
                    inst_name,
                    lsb,
                    msb,
                } = child
                {
                    let nbits = *msb - *lsb + 1;
                    let mask = ((1 << nbits) - 1) << *lsb;
                    writeln!(
                        &mut output,
                        r##"
        #[allow(dead_code)]
        #[allow(non_upper_case_globals)]
        pub const {}: u8 = 0b{:08b};
"##,
                        inst_name, mask
                    )?;
                } else {
                    panic!("unexpected non-Field: {:?}", child);
                }
            }

            writeln!(&mut output, "    }}\n")?;
        } else {
            panic!("unexpected child {:?}", child);
        }
    }

    writeln!(&mut output, "}}")?;

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
