// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Result;
use serde::Deserialize;
use std::collections::BTreeMap;

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
    /// during the `net` build, so it must be present iff the `vlan` feature
    /// is turned on.
    pub vlan: Option<VLanConfig>,
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
}

#[derive(Copy, Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VLanConfig {
    /// Address of the 0-index VLAN
    pub start: usize,
    /// Number of VLANs
    pub count: usize,
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

    match (cfg!(feature = "vlan"), cfg.vlan.is_some()) {
        (true, false) => {
            panic!("VLAN feature is enabled, but vlan is missing from config")
        }
        (false, true) => {
            panic!("VLAN feature is disabled, but vlan is present in config")
        }
        _ => (),
    }

    Ok(cfg)
}

pub fn generate_vlan_consts(
    config: &NetConfig,
    mut out: impl std::io::Write,
) -> Result<(), std::io::Error> {
    let vlan = config.vlan.unwrap();
    let end = vlan.start + vlan.count;
    if end > 0xFFF {
        panic!("Invalid VLAN range (must be < 4096)");
    }
    writeln!(
        out,
        "
pub const VLAN_RANGE: core::ops::Range<u16> = {:#x}..{:#x};
pub const VLAN_COUNT: usize = {};
",
        vlan.start, end, vlan.count
    )
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

    Ok(())
}
