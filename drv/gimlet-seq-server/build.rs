// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::fmt::Write;
use std::{env, fs, path::PathBuf};

#[derive(serde::Deserialize)]
struct Config {
    fpga_image: String,
    register_defs: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_target_board();

    let config = build_util::task_config::<Config>()?;

    let fpga_image_path = PathBuf::from(&config.fpga_image);

    if fpga_image_path.components().count() != 1 {
        panic!("fpga_image path mustn't contain a slash, sorry.");
    }

    let fpga_image = fs::read(&fpga_image_path)?;
    let compressed = compress(&fpga_image);

    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let compressed_path = out.join(fpga_image_path.with_extension("bin.rle"));
    fs::write(&compressed_path, compressed)?;
    println!("cargo:rerun-if-changed={}", config.fpga_image);

    println!(
        "cargo:rustc-env=GIMLET_FPGA_IMAGE_PATH={}",
        compressed_path.display()
    );

    let disposition = build_i2c::Disposition::Devices;

    if let Err(e) = build_i2c::codegen(disposition) {
        println!("code generation failed: {}", e);
        std::process::exit(1);
    }

    let regs_in = PathBuf::from(config.register_defs);
    let regs_out = out.join(regs_in.with_extension("rs"));
    fs::write(&regs_out, regs(regs_in)?)?;
    println!("cargo:rustc-env=GIMLET_FPGA_REGS={}", regs_out.display());

    idol::server::build_server_support(
        "../../idl/gimlet-seq.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

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

fn regs(defs: PathBuf) -> Result<String, Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed={}", defs.display());
    let input = String::from_utf8(fs::read(&defs)?)?;

    let mut output = String::new();

    let node: Node = serde_json::from_str(&input)?;

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
            writeln!(&mut output, "    {} = {:#x},", inst_name, addr_offset)?;
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
