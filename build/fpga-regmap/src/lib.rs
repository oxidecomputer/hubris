// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use convert_case::{Case, Casing};
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
#[derive(Copy, Clone, PartialEq)]
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

    writeln!(
        output,
        "
impl Addr {{
    /// Returns true iff this `Addr` immediately precedes the parameter,
    /// which can be useful to statically assert that multibyte reads will
    /// read the desired registers.
    pub const fn precedes(self, other: Addr) -> bool {{
        self as u16 + 1 == other as u16
    }}
}}"
    )
    .unwrap();
}

////////////////////////////////////////////////////////////////////////////////

fn write_reg_fields(
    parents: Vec<String>,
    children: &[Node],
    prefix: &str,
    output: &mut String,
) {
    // We need this to implement the u8 -> enum conversion
    let parent_chain = parents.join("::");

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
            if let Some(x) = encode {
                let name_camel = inst_name.to_case(Case::UpperCamel);
                let encode_name = format!("{name_camel}Encoded");
                writeln!(
                    output,
                    "
{prefix}        #[derive(Copy, Clone, Eq, PartialEq)]
{prefix}        #[allow(dead_code)]
{prefix}        pub enum {encode_name} {{"
                )
                .unwrap();

                // unpack all of the enum variant -> u8 information
                for item in x {
                    writeln!(
                        output,
                        "{prefix}            {0} = {1:#04x},",
                        item.name.to_case(Case::UpperCamel),
                        item.value
                    )
                    .unwrap();
                }

                writeln!(output, "{prefix}        }}").unwrap();

                // We want to implement TryFrom<u8> rather than From<u8>
                // because the u8 -> enum conversion can fail if the value
                // is not a valid enum variant. Additionally, we mask off
                // the supplied u8 with the mask of the field so encoded
                // fields could be colocated in a register with other fields
                writeln!(
                    output,
                    "
{prefix}        impl TryFrom<u8> for {encode_name} {{
{prefix}            type Error = ();
{prefix}            fn try_from(x: u8) -> Result<Self, Self::Error> {{
{prefix}                use crate::{parent_chain}::{encode_name}::*;
{prefix}                let x_masked = x & {inst_name};
{prefix}                match x_masked {{"
                )
                .unwrap();
                for item in x {
                    writeln!(
                        output,
                        "{prefix}                    {1:#04x} => Ok({0}),",
                        item.name.to_case(Case::UpperCamel),
                        item.value
                    )
                    .unwrap();
                }
                writeln!(
                    output,
                    "{prefix}                    _ => Err(()),
{prefix}                }}
{prefix}            }}
{prefix}        }}\n"
                )
                .unwrap();
            }
        } else {
            panic!("unexpected non-Field: {child:?}");
        }
    }
}

fn write_node(
    parents: &[String],
    node: &Node,
    prefix: &str,
    output: &mut String,
) {
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

            // Extend the knowledge of parents as we descend
            let mut new_parents = parents.to_owned();
            new_parents.push(inst_name.clone());
            write_reg_fields(new_parents, children, prefix, output);

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

            let mut new_parents = parents.to_owned();
            new_parents.push(inst_name.clone());
            recurse_reg_map(
                &new_parents,
                children,
                &format!("    {prefix}"),
                output,
            );
            writeln!(output, "{prefix}    }}").unwrap();
        }

        _ => {
            panic!("unexpected child {node:?}");
        }
    }
}

fn recurse_reg_map(
    parents: &[String],
    children: &[Node],
    prefix: &str,
    output: &mut String,
) {
    for child in children.iter() {
        write_node(parents, child, prefix, output);
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

    // The nested layers may require type information that requires knowledge
    // of where they are in the tree.
    let root = vec!["Reg".to_string()];
    recurse_reg_map(&root, children, "", output);

    writeln!(output, "}}").unwrap();
}

////////////////////////////////////////////////////////////////////////////////

pub fn fpga_regs(
    regs: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut output = String::new();

    let node: Node = serde_json::from_str(regs)?;

    writeln!(&mut output, "// Auto-generated code, do not modify!").unwrap();
    build_addr_map(&node, &mut output);
    build_reg_map(&node, &mut output);

    Ok(output)
}
