// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::fmt::Write;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Node {
    Addrmap {
        inst_name: String,
        addr_offset: usize,
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
        encode: Option<Vec<EnumEncode>>,
    },
    Mem {
        inst_name: String,
        addr_offset: usize,
    },
}

#[derive(Debug, Deserialize)]
struct EnumEncode {
    name: String,
    value: u8,
}

////////////////////////////////////////////////////////////////////////////////

fn recurse_addr_map(
    children: &[Node],
    offset: usize,
    prefix: &str,
    output: &mut String,
) {
    for child in children.iter() {
        match child {
            Node::Reg {
                inst_name,
                addr_offset,
                ..
            } => {
                writeln!(
                    output,
                    "    {prefix}{inst_name} = {:#x},",
                    offset + addr_offset
                )
                .unwrap();
            }
            Node::Addrmap {
                inst_name,
                addr_offset,
                children,
            } => {
                recurse_addr_map(
                    children,
                    offset + addr_offset,
                    &format!("{inst_name}_{prefix}"),
                    output,
                );
            }
            Node::Mem {
                inst_name,
                addr_offset,
                ..
            } => {
                writeln!(
                    output,
                    "    {prefix}{inst_name} = {:#x},",
                    offset + addr_offset
                )
                .unwrap();
            }
            _ => panic!("unexpected child {:?}", child),
        }
    }
}

fn build_addr_map(node: &Node, output: &mut String) {
    let children = if let Node::Addrmap { children, .. } = node {
        children
    } else {
        panic!("top-level node is not addrmap");
    };

    writeln!(
        output,
        "\
#[derive(Copy, Clone)]
#[allow(non_camel_case_types)]
#[allow(clippy::upper_case_acronyms)]
pub enum Addr {{"
    )
    .unwrap();

    recurse_addr_map(children, 0, "", output);

    writeln!(output, "}}").unwrap();
    writeln!(
        output,
        "
impl From<Addr> for u16 {{
    fn from(a: Addr) -> Self {{
        a as u16
    }}
}}"
    )
    .unwrap();
}

////////////////////////////////////////////////////////////////////////////////

fn write_reg_fields(children: &[Node], prefix: &str, output: &mut String) {
    for child in children.iter() {
        if let Node::Field {
            inst_name,
            lsb,
            msb,
            encode,
        } = child
        {
            let nbits = *msb - *lsb + 1;
            let mask = ((1 << nbits) - 1) << *lsb;
            writeln!(
                output,
                "\
{prefix}        #[allow(dead_code)]
{prefix}        #[allow(non_upper_case_globals)]
{prefix}        pub const {inst_name}: u8 = 0b{mask:08b};",
            )
            .unwrap();
            // Deal with optional encoded Enums on this field
            match encode {
                Some(x) => {
                    writeln!(
                        output,
                        "
{prefix}        use num_derive::{{ToPrimitive, FromPrimitive}};
{prefix}        #[derive(FromPrimitive, ToPrimitive)]
{prefix}        #[allow(dead_code)]
{prefix}        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
{prefix}        pub enum Encoded {{"
                    )
                    .unwrap();
                    for item in x {
                        writeln!(
                            output,
                            "{prefix}            {0} = {1:#04x},",
                            item.name, item.value
                        )
                        .unwrap();
                    }
                    writeln!(output, "{prefix}        }}").unwrap();
                }
                None => {}
            }
        } else {
            panic!("unexpected non-Field: {child:?}");
        }
    }
}

fn write_node(node: &Node, prefix: &str, output: &mut String) {
    match node {
        Node::Reg {
            inst_name,
            regwidth,
            children,
            ..
        } => {
            if *regwidth != 8 {
                panic!("only 8-bit registers supported");
            }

            writeln!(
                output,
                "\
{prefix}    #[allow(non_snake_case)]
{prefix}    pub mod {inst_name} {{",
            )
            .unwrap();
            write_reg_fields(children, prefix, output);

            writeln!(output, "{prefix}    }}").unwrap();
        }

        Node::Mem { inst_name, .. } => {
            writeln!(
                output,
                "\
{prefix}    #[allow(non_snake_case)]
{prefix}    pub mod {inst_name} {{",
            )
            .unwrap();

            writeln!(output, "{prefix}    }}").unwrap();
        }

        // Recurse into Addrmap
        Node::Addrmap {
            inst_name,
            children,
            ..
        } => {
            writeln!(
                output,
                "\
{prefix}    #[allow(non_snake_case)]
{prefix}    pub mod {inst_name} {{",
            )
            .unwrap();
            recurse_reg_map(children, &format!("    {prefix}"), output);
            writeln!(output, "{prefix}    }}").unwrap();
        }

        _ => {
            panic!("unexpected child {node:?}");
        }
    }
}

fn recurse_reg_map(children: &[Node], prefix: &str, output: &mut String) {
    for child in children.iter() {
        write_node(child, prefix, output);
    }
}

fn build_reg_map(node: &Node, output: &mut String) {
    let children = if let Node::Addrmap { children, .. } = node {
        children
    } else {
        panic!("top-level node is not addrmap");
    };

    writeln!(
        output,
        "
#[allow(non_snake_case)]
pub mod Reg {{"
    )
    .unwrap();

    recurse_reg_map(children, "", output);

    writeln!(output, "}}").unwrap();
}

////////////////////////////////////////////////////////////////////////////////

pub fn fpga_regs(regs: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut output = String::new();

    let node: Node = serde_json::from_str(regs)?;

    writeln!(&mut output, "// Auto-generated code, do not modify!").unwrap();
    build_addr_map(&node, &mut output);
    build_reg_map(&node, &mut output);

    Ok(output)
}
