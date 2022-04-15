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
    pub vlan_start: Option<u16>,

    /// Number of VLANs
    pub vlan_count: Option<u16>,
}

impl NetConfig {
    pub fn instances(&self) -> usize {
        match self.vlan_count {
            Some(i) => i as usize,
            None => 1,
        }
    }
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
    Ok(build_util::config::<GlobalConfig>()?.net)
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
