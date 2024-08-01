// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Result;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

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
    #[serde(default)]
    pub vlans: Vec<VLanConfig>,
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
        .iter()
        .map(|v| v.port)
        .collect::<BTreeSet<_>>()
        .len();
    assert!(ports <= 2);
    writeln!(out, "pub const PORT_COUNT: usize = {};", ports.max(1))
}

pub fn generate_vlan_consts(
    config: &NetConfig,
    mut out: impl std::io::Write,
) -> Result<(), std::io::Error> {
    if config.vlans.iter().any(|v| v.vid > 0xFFF) {
        panic!("Invalid VLAN VID (must be < 4096)");
    }
    writeln!(
        out,
        "
    pub const VLAN_COUNT: usize = {0};
    pub const VLANS: [VLanConfig; {0}] = [",
        config.vlans.len()
    )?;
    for v in &config.vlans {
        writeln!(
            out,
            "
    VLanConfig {{
        vid: {:#x},
        trusted: {},
        port: {}
    }},",
            v.vid,
            v.trusted,
            match v.port {
                1 => "SpPort::One",
                2 => "SpPort::Two",
                _ => panic!("invalid SP port, must be 1 or 2"),
            }
        )?;
    }
    writeln!(out, "];")?;
    write!(
        out,
        "
    pub const VLAN_VIDS: [u16; {0}] = [",
        config.vlans.len()
    )?;
    for v in &config.vlans {
        write!(out, "{:#x},", v.vid)?;
    }
    writeln!(out, "];")?;

    Ok(())
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
