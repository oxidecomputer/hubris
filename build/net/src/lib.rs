// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::collections::BTreeMap;

///////////////////////////////////////////////////////////////////////////////
// Network config schema definition.
//

#[derive(Deserialize)]
pub struct GlobalConfig {
    pub net: NetConfig,
}

#[derive(Deserialize)]
pub struct NetConfig {
    /// Sockets known to the system, indexed by name.
    pub sockets: BTreeMap<String, SocketConfig>,

    /// Address of the lowest VLAN
    pub vlan_start: Option<usize>,

    /// Number of VLANs
    pub vlan_count: Option<usize>,
}

/// TODO: this type really wants to be an enum, but the toml crate's enum
/// handling is really, really fragile, and currently it would be an enum with a
/// single variant anyway.
#[derive(Deserialize)]
pub struct SocketConfig {
    pub kind: String,
    pub owner: TaskNote,
    pub port: u16,
    pub tx: BufSize,
    pub rx: BufSize,
}

#[derive(Deserialize)]
pub struct BufSize {
    pub packets: usize,
    pub bytes: usize,
}

#[derive(Deserialize)]
pub struct TaskNote {
    pub name: String,
    pub notification: u32,
}

pub fn load_net_config() -> Result<NetConfig, Box<dyn std::error::Error>> {
    let cfg = build_util::config::<GlobalConfig>()?.net;

    #[cfg(feature = "vlan")]
    {
        if cfg.vlan_count.is_none() {
            panic!("VLAN feature is enabled, but vlan_count is missing from config");
        } else if cfg.vlan_start.is_none() {
            panic!("VLAN feature is enabled, but vlan_start is missing from config");
        }
    }
    #[cfg(not(feature = "vlan"))]
    {
        if cfg.vlan_count.is_some() {
            panic!(
                "VLAN feature is disabled, but vlan_count is present in config"
            );
        } else if cfg.vlan_start.is_some() {
            panic!(
                "VLAN feature is disabled, but vlan_start is present in config"
            );
        }
    }

    Ok(cfg)
}

pub fn generate_vlan_consts(
    config: &NetConfig,
    mut out: impl std::io::Write,
) -> Result<(), std::io::Error> {
    writeln!(
        out,
        "pub const VLAN_START: usize = {}; pub const VLAN_COUNT: usize = {};",
        config.vlan_start.unwrap(),
        config.vlan_count.unwrap()
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
    writeln!(out, "#[derive(serde::Serialize, serde::Deserialize)]")?;
    writeln!(out, "pub enum SocketName {{")?;
    for (i, name) in config.sockets.keys().enumerate() {
        writeln!(out, "    {} = {},", name, i)?;
    }
    writeln!(out, "}}")?;
    Ok(())
}
