// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Result;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

use convert_case::{Case, Casing};
use proc_macro2::TokenStream;
use quote::quote;

///////////////////////////////////////////////////////////////////////////////
// Network config schema definition.
//

/// This represents our _subset_ of global config and _must not_ be marked with
/// `deny_unknown_fields`!
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GlobalConfig {
    pub net: NetConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NetConfig {
    /// Sockets known to the system, indexed by name.
    pub sockets: BTreeMap<String, SocketConfig>,

    /// VLAN configuration, or None. This is checked against enabled features
    /// during the `net` build, so it must be non-empty iff the `vlan` feature
    /// is turned on.
    ///
    /// MAC addresses are assigned based on order in the `enum VLanId`; we'll
    /// use an `IndexMap` here to match the order from the TOML file for
    /// consistency.
    #[serde(default)]
    pub vlans: indexmap::IndexMap<String, VLanConfig>,
}

/// TODO: this type really wants to be an enum, but the toml crate's enum
/// handling is really, really fragile, and currently it would be an enum with a
/// single variant anyway.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SocketConfig {
    pub kind: String,
    pub owner: TaskNote,
    pub port: u16,
    pub tx: BufSize,
    pub rx: BufSize,

    #[serde(default)]
    pub allow_untrusted: bool,
}

#[derive(Copy, Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VLanConfig {
    /// VLAN VID
    pub vid: u16,

    /// Whether this VLAN is initially trusted
    pub trusted: bool,

    /// Equivalent SP port (one or two)
    pub port: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BufSize {
    pub packets: usize,
    pub bytes: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TaskNote {
    pub name: String,
    pub notification: String,
}

pub fn load_net_config() -> Result<NetConfig> {
    let cfg = build_util::config::<GlobalConfig>()?.net;

    match (cfg!(feature = "vlan"), cfg.vlans.is_empty()) {
        (true, true) => {
            panic!("VLAN feature is enabled, but vlan is missing from config")
        }
        (false, false) => {
            panic!("VLAN feature is disabled, but vlan is present in config")
        }
        _ => (),
    }

    Ok(cfg)
}

pub fn generate_port_consts(
    config: &NetConfig,
    mut out: impl std::io::Write,
) -> Result<(), std::io::Error> {
    let ports = config
        .vlans
        .values()
        .map(|v| v.port)
        .collect::<BTreeSet<_>>()
        .len();
    assert!(ports <= 2);
    let port_count = ports.max(1);
    let s = quote! {
        pub const PORT_COUNT: usize = #port_count;
    };
    writeln!(out, "{s}")
}

fn check_vlan_config(config: &NetConfig) {
    if let Some(v) = config.vlans.values().find(|v| v.vid > 0xFFF) {
        panic!("Invalid VLAN VID {} (must be < 4096)", v.vid);
    }
    for (k1, v1) in &config.vlans {
        for (k2, v2) in &config.vlans {
            if k1 != k2 && v1.vid == v2.vid {
                panic!("Duplicate VID {} (in {k1} and {k2})", v1.vid);
            }
        }
    }
}

pub fn generate_vlan_consts(
    config: &NetConfig,
    mut out: impl std::io::Write,
) -> Result<(), std::io::Error> {
    check_vlan_config(config);

    let vlan_count = config.vlans.len();
    let vids = config.vlans.values().map(|cfg| cfg.vid).collect::<Vec<_>>();
    let s = quote! {
        #[allow(unused)]
        pub const VLAN_VIDS: [u16; #vlan_count] = [
            #(
                #vids
            ),*
        ];
    };
    writeln!(out, "{s}")
}

pub fn generate_vlan_enum(
    config: &NetConfig,
    mut out: impl std::io::Write,
) -> Result<(), std::io::Error> {
    check_vlan_config(config);

    let s = if config.vlans.is_empty() {
        quote! {
            #[derive(
                Copy, Clone, Eq, PartialEq,
                enum_map::Enum,
                serde::Serialize, serde::Deserialize,
                hubpack::SerializedSize,
                counters::Count,
            )]
            pub enum VLanId {
                None,
            }
        }
    } else {
        let names = config
            .vlans
            .keys()
            .map(|b| -> TokenStream {
                b.to_case(Case::UpperCamel).parse().unwrap()
            })
            .collect::<Vec<_>>();
        let cfgs = config
            .vlans
            .values()
            .map(|cfg| {
                let vid = cfg.vid;
                let always_trusted = cfg.trusted;
                let port = match cfg.port {
                    1 => quote! { SpPort::One },
                    2 => quote! { SpPort::Two },
                    _ => panic!("invalid SP port, must be 1 or 2"),
                };
                quote! {
                    VLanConfig {
                        vid: #vid,
                        always_trusted: #always_trusted,
                        port: #port,
                    }
                }
            })
            .collect::<Vec<_>>();
        quote! {
            #[derive(
                Copy, Clone, Eq, PartialEq,
                enum_map::Enum,
                serde::Serialize, serde::Deserialize,
                hubpack::SerializedSize,
                counters::Count,
            )]
            pub enum VLanId {
                #(#names),*
            }

            impl VLanId {
                pub fn cfg(&self) -> VLanConfig {
                    match self {
                    #(
                        VLanId::#names => #cfgs
                    ),*
                    }
                }
            }
        }
    };

    writeln!(out, "{s}")
}

pub fn generate_socket_enum(
    config: &NetConfig,
    mut out: impl std::io::Write,
) -> Result<(), std::io::Error> {
    writeln!(out, "#[allow(non_camel_case_types)]")?;
    writeln!(out, "#[repr(u8)]")?;
    writeln!(
        out,
        "#[derive(Copy, Clone, Debug, Eq, PartialEq, userlib::FromPrimitive)]"
    )?;
    writeln!(
        out,
        "#[derive(serde::Serialize, \
                  serde::Deserialize, \
                  hubpack::SerializedSize)]"
    )?;
    // The `net` task itself doesn't use this, but its clients do...
    writeln!(out, "#[allow(dead_code)]")?;
    writeln!(out, "pub enum SocketName {{")?;
    for (i, name) in config.sockets.keys().enumerate() {
        writeln!(out, "    {} = {},", name, i)?;
    }
    writeln!(out, "}}")?;

    writeln!(
        out,
        "#[allow(unused)]\
        pub const SOCKET_TX_SIZE: [usize; {}] = [",
        config.sockets.len(),
    )?;
    for c in config.sockets.values() {
        writeln!(out, "{},", c.tx.bytes)?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "#[allow(unused)]\
        pub const SOCKET_RX_SIZE: [usize; {}] = [",
        config.sockets.len(),
    )?;
    for c in config.sockets.values() {
        writeln!(out, "{},", c.rx.bytes)?;
    }
    writeln!(out, "];")?;

    writeln!(
        out,
        "#[allow(unused)]\
        pub const SOCKET_ALLOW_UNTRUSTED: [bool; {}] = [",
        config.sockets.len(),
    )?;
    for c in config.sockets.values() {
        writeln!(out, "{},", c.allow_untrusted)?;
    }
    writeln!(out, "];")?;

    Ok(())
}
