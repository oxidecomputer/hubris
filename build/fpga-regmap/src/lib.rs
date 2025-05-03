// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{bail, Context};
use convert_case::{Case, Casing};
use serde::Deserialize;
use serde_with::{serde_as, DefaultOnNull};
use std::{fmt::Write, path::PathBuf};

#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Node {
    Addrmap {
        inst_name: String,
        addr_offset: usize,
        children: Vec<Node>,
        orig_type_name: Option<String>,
        addr_span_bytes: Option<usize>,
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
        sw_access: SwAccess,
        #[serde_as(as = "DefaultOnNull")]
        desc: String,
    },
    Mem {
        inst_name: String,
        addr_offset: usize,
    },
}

#[derive(Debug, Deserialize)]
pub struct EnumEncode {
    name: String,
    value: u8,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SwAccess {
    #[serde(alias = "r")]
    Read,
    #[serde(alias = "w")]
    Write,
    #[serde(alias = "rw")]
    ReadWrite,
}

impl SwAccess {
    fn is_read(&self) -> bool {
        matches!(self, SwAccess::Read | SwAccess::ReadWrite)
    }
    fn is_write(&self) -> bool {
        matches!(self, SwAccess::Write | SwAccess::ReadWrite)
    }
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
                ..
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

pub fn build_addr_map(node: &Node, output: &mut String) {
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
            ..
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

pub fn build_reg_map(node: &Node, output: &mut String) {
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

////////////////////////////////////////////////////////////////////////////////

pub fn build_peripheral(
    node: &Node,
    top: &Node,
    peripheral: &str,
    base_addr: u32,
    token: Option<&str>,
) -> anyhow::Result<String> {
    use heck::{ToSnakeCase, ToUpperCamelCase};
    use quote::quote;

    let periph_offset = match top {
        Node::Addrmap { children, .. } => {
            let Some(offset) = children.iter().find_map(|c| match c {
                Node::Addrmap {
                    inst_name,
                    addr_offset,
                    ..
                } if inst_name == peripheral => Some(addr_offset),
                _ => None,
            }) else {
                panic!("could not find '{peripheral}' in top map");
            };
            offset
        }
        _ => panic!("top node must be an addrmap"),
    };

    let Node::Addrmap { children, .. } = node else {
        panic!("peripheral node must be an addrmap");
    };
    let mut reg_definitions = vec![];
    let mut reg_types = vec![];
    let mut reg_decls = vec![];
    for c in children {
        let Node::Reg {
            inst_name,
            addr_offset,
            regwidth,
            children,
        } = c
        else {
            panic!("nodes within map must be registers, not {c:?}");
        };
        assert_eq!(*regwidth, 32, "only 32-bit registers are supported");
        let mut struct_fns = vec![];
        let mut debug_values = vec![];
        let mut debug_names = vec![];
        let mut debug_types = vec![];
        let mut encode_types = vec![];
        for c in children {
            let Node::Field {
                inst_name,
                lsb,
                msb,
                encode,
                sw_access,
                desc,
            } = c
            else {
                panic!("nodes within register must be fields, not {c:?}");
            };
            let msb = u32::try_from(*msb).unwrap();
            let lsb = u32::try_from(*lsb).unwrap();
            let setter: syn::Ident =
                syn::parse_str(&format!("set_{}", inst_name.to_snake_case()))
                    .unwrap();
            let getter: syn::Ident =
                syn::parse_str(&inst_name.to_snake_case()).unwrap();
            if lsb == msb {
                if sw_access.is_write() {
                    struct_fns.push(quote! {
                        #[doc = #desc]
                        pub fn #setter(&self, t: bool) {
                            let mut d = self.get_raw();
                            if t {
                                d |= 1 << #msb;
                            } else {
                                d &= !(1 << #msb);
                            }
                            self.set_raw(d);
                        }
                    });
                }
                if sw_access.is_read() {
                    struct_fns.push(quote! {
                        #[doc = #desc]
                        pub fn #getter(&self) -> bool {
                            let d = self.get_raw();
                            (d & (1 << #msb)) != 0
                        }
                    });
                    debug_values.push(quote! {
                        let #getter = (d & (1 << #msb)) != 0;
                    });
                    debug_names.push(quote! { #getter });
                    debug_types.push(quote! { pub #getter: bool });
                }
            } else if let Some(encode) = encode {
                let ty: syn::Ident =
                    syn::parse_str(&inst_name.to_upper_camel_case()).unwrap();
                let width = msb - lsb + 1;
                let mask = u32::try_from((1u64 << width) - 1).unwrap();
                let raw_ty = match width {
                    1 => unreachable!("1-bit integers should be bools"),
                    2..=8 => "u8",
                    9..=16 => "u16",
                    17..=32 => "u32",
                    _ => panic!("invalid width {width}"),
                };
                assert_eq!(width, 8, "EnumEncode must be 8 bits wide");
                let quoted = encode
                    .iter()
                    .map(|e| {
                        let v: syn::Ident =
                            syn::parse_str(&e.name.to_upper_camel_case())
                                .unwrap();
                        let i: syn::LitInt =
                            syn::parse_str(&format!("{}{raw_ty}", e.value))
                                .unwrap();
                        (v, i)
                    })
                    .collect::<Vec<_>>();

                let variants = quoted.iter().map(|(v, i)| quote! { #v = #i });
                let matches =
                    quoted.iter().map(|(v, i)| quote! { #i => Ok(Self::#v), });
                let raw_ty: syn::Ident = syn::parse_str(raw_ty).unwrap();
                encode_types.push(quote! {
                    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
                    #[repr(#raw_ty)]
                    pub enum #ty {
                        #(#variants),*
                    }

                    impl core::convert::TryFrom<#raw_ty> for #ty {
                        type Error = #raw_ty;
                        fn try_from(t: #raw_ty) -> Result<Self, Self::Error> {
                            match t {
                                #(#matches)*
                                _ => Err(t),
                            }
                        }
                    }
                });
                if sw_access.is_write() {
                    struct_fns.push(quote! {
                        #[doc = #desc]
                        pub fn #setter(&self, t: #ty) {
                            let mut d = self.get_raw();
                            d &= !(#mask << #lsb);
                            d |= (u32::from(t as #raw_ty) & #mask) << #lsb;
                            self.set_raw(d);
                        }
                    });
                }
                if sw_access.is_read() {
                    struct_fns.push(quote! {
                        #[doc = #desc]
                        pub fn #getter(&self) -> Result<#ty, #raw_ty> {
                            let d = self.get_raw();
                            let t = ((d >> #lsb) & #mask) as #raw_ty;
                            #ty::try_from(t)
                        }
                    });
                    debug_values.push(quote! {
                        let t = ((d >> #lsb) & #mask) as #raw_ty;
                        let #getter = #ty::try_from(t);
                    });
                    debug_names.push(quote! { #getter });
                    debug_types
                        .push(quote! { pub #getter: Result<#ty, #raw_ty> });
                }
            } else {
                let width = msb - lsb + 1;
                let mask = u32::try_from((1u64 << width) - 1).unwrap();
                let ty = match width {
                    1 => unreachable!("1-bit integers should be bools"),
                    2..=8 => "u8",
                    9..=16 => "u16",
                    17..=32 => "u32",
                    _ => panic!("invalid width {width}"),
                };
                let ty: syn::Ident = syn::parse_str(ty).unwrap();
                if sw_access.is_write() {
                    struct_fns.push(quote! {
                        #[doc = #desc]
                        pub fn #setter(&self, t: #ty) {
                            let mut d = self.get_raw();
                            d &= !(#mask << #lsb);
                            d |= (u32::from(t) & #mask) << #lsb;
                            self.set_raw(d);
                        }
                    });
                }
                if sw_access.is_read() {
                    struct_fns.push(quote! {
                        #[doc = #desc]
                        pub fn #getter(&self) -> #ty {
                            let d = self.get_raw();
                            ((d >> #lsb) & #mask) as #ty
                        }
                    });
                    debug_values.push(quote! {
                        let #getter = ((d >> #lsb) & #mask) as #ty;
                    });
                    debug_names.push(quote! { #getter });
                    debug_types.push(quote! { pub #getter: #ty });
                }
            }
        }

        let inst_name = inst_name.to_upper_camel_case();
        let struct_name: syn::Ident = syn::parse_str(&inst_name).unwrap();
        let handle_name: syn::Ident =
            syn::parse_str(&format!("{}Handle", inst_name)).unwrap();
        let debug_name: syn::Ident =
            syn::parse_str(&format!("{}Debug", inst_name)).unwrap();
        let reg_addr = base_addr
            + u32::try_from(*periph_offset).unwrap()
            + u32::try_from(*addr_offset).unwrap();
        let struct_def = quote! {
            pub struct #struct_name;
            #[allow(dead_code, clippy::useless_conversion, clippy::unnecessary_cast)]
            impl #struct_name {
                const ADDR: *mut u32 = #reg_addr as *mut u32;
                fn new() -> Self {
                    #struct_name
                }
                fn get_raw(&self) -> u32 {
                    unsafe {
                        Self::ADDR.read_volatile()
                    }
                }
                fn set_raw(&self, v: u32) {
                    unsafe {
                        Self::ADDR.write_volatile(v)
                    }
                }
                pub fn modify<F: Fn(&mut #handle_name)>(&self, f: F) {
                    let mut v =
                        #handle_name(core::cell::Cell::new(self.get_raw()));
                    f(&mut v);
                    self.set_raw(v.0.get());
                }

                #(#struct_fns)*
            }

            pub struct #handle_name(core::cell::Cell<u32>);
            #[allow(dead_code, clippy::useless_conversion, clippy::unnecessary_cast)]
            impl #handle_name {
                fn get_raw(&self) -> u32 {
                    self.0.get()
                }
                fn set_raw(&self, v: u32) {
                    self.0.set(v)
                }
                #(#struct_fns)*
            }

            #[derive(Copy, Clone, Eq, PartialEq)]
            #[allow(dead_code, clippy::useless_conversion, clippy::unnecessary_cast)]
            pub struct #debug_name {
                #(#debug_types),*
            }
            #[allow(dead_code, clippy::useless_conversion, clippy::unnecessary_cast)]
            impl<'a> From<&'a #struct_name> for #debug_name {
                fn from(s: &'a #struct_name) -> #debug_name {
                    let d = s.get_raw();
                    #(#debug_values)*
                    #debug_name {
                        #(#debug_names),*
                    }
                }
            }

            #(#encode_types)*
        };
        reg_definitions.push(struct_def);
        let reg_name: syn::Ident =
            syn::parse_str(&inst_name.to_snake_case()).unwrap();
        reg_types.push(quote! {
            pub #reg_name: #struct_name
        });
        reg_decls.push(quote! {
            #reg_name: #struct_name::new()
        });
    }

    let periph_name: syn::Ident =
        syn::parse_str(&peripheral.to_upper_camel_case()).unwrap();
    let peripheral_def = if let Some(token) = token {
        let token_ty: syn::Path = syn::parse_str(token).unwrap();
        quote! {
            #[allow(dead_code)]
            pub struct #periph_name {
                #(#reg_types),*
            }
            #[allow(dead_code)]
            impl #periph_name {
                pub fn new(_token: #token_ty) -> Self {
                    Self {
                        #(#reg_decls),*
                    }
                }
            }
            #(#reg_definitions)*
        }
    } else {
        quote! {
            #[allow(dead_code)]
            pub struct #periph_name {
                #(#reg_types),*
            }
            #[allow(dead_code)]
            impl #periph_name {
                pub fn new() -> Self {
                    Self {
                        #(#reg_decls),*
                    }
                }
            }
            #(#reg_definitions)*
        }
    };

    let f: syn::File = syn::parse2(peripheral_def).unwrap();
    Ok(prettyplease::unparse(&f))
}

/// Read and parse a JSON file containing a `Node`
pub fn read_parse(p: &std::path::Path) -> anyhow::Result<Node> {
    use std::io::Read;
    let mut data = vec![];
    std::fs::File::open(p)
        .with_context(|| format!("failed to open {p:?}"))?
        .read_to_end(&mut data)?;
    let src = std::str::from_utf8(&data)?;
    let node: Node = serde_json::from_str(src)?;
    Ok(node)
}

/// Generates an FPGA peripheral for the given node
///
/// The register map and base address are loaded from environmental variables,
/// so this must be called in the context of a Hubris task build.
pub fn fpga_peripheral(name: &str, token: &str) -> anyhow::Result<String> {
    let base_addr: u32 = build_util::env_var("HUBRIS_MMIO_BASE_ADDRESS")?
        .parse()
        .context("parsing base address")?;
    let reg_map =
        PathBuf::from(build_util::env_var("HUBRIS_MMIO_REGISTER_MAP")?);

    let top = read_parse(&reg_map)?;

    let Node::Addrmap { children, .. } = &top else {
        bail!("expected addrmap for top, got {top:?}");
    };
    let Some(orig_type_name) = children.iter().find_map(|c| match c {
        Node::Addrmap {
            inst_name,
            orig_type_name,
            ..
        } if inst_name == name => {
            assert!(orig_type_name.is_some(), "must provide orig_type_name");
            orig_type_name.as_deref()
        }
        _ => None,
    }) else {
        bail!("could not find peripheral with `inst_name` of {name}");
    };

    let node_name = reg_map
        .parent()
        .unwrap()
        .join(format!("{orig_type_name}.json"));
    let node = read_parse(&node_name)?;

    build_peripheral(&node, &top, name, base_addr, Some(token))
}
