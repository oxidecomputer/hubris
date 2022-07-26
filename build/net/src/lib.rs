// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;

///////////////////////////////////////////////////////////////////////////////
// Network config schema definition.
//

/// This represents our _subset_ of global config and _must not_ be marked with
/// `deny_unknown_fields`!
#[derive(Deserialize, knuffel::Decode)]
#[serde(rename_all = "kebab-case")]
pub struct GlobalConfig {
    #[knuffel(child)]
    pub net: NetConfig,
}

#[derive(Deserialize, knuffel::Decode)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NetConfig {
    /// Sockets known to the system.
    #[knuffel(children(name = "socket"))]
    pub sockets: Vec<SocketConfig>,

    /// VLAN configuration, or None. This is checked against enabled features
    /// during the `net` build, so it must be present iff the `vlan` feature
    /// is turned on.
    #[knuffel(child)]
    pub vlan: Option<VLanConfig>,
}

/// TODO: this type really wants to be an enum, but the toml crate's enum
/// handling is really, really fragile, and currently it would be an enum with a
/// single variant anyway.
#[derive(Deserialize, knuffel::Decode)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SocketConfig {
    #[knuffel(argument)]
    pub name: String,
    #[knuffel(child, unwrap(argument))]
    pub kind: String,
    #[knuffel(child)]
    pub owner: TaskNote,
    #[knuffel(child, unwrap(argument))]
    pub port: u16,
    #[knuffel(child)]
    pub tx: BufSize,
    #[knuffel(child)]
    pub rx: BufSize,
}

#[derive(Copy, Clone, Debug, Deserialize, knuffel::Decode)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VLanConfig {
    /// Address of the 0-index VLAN
    #[knuffel(property)]
    pub start: usize,
    /// Number of VLANs
    #[knuffel(property)]
    pub count: usize,
}

#[derive(Deserialize, knuffel::Decode)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BufSize {
    #[knuffel(property)]
    pub packets: usize,
    #[knuffel(property)]
    pub bytes: usize,
}

#[derive(Deserialize, knuffel::Decode)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TaskNote {
    #[knuffel(argument)]
    pub name: String,
    #[knuffel(property(name = "mask"))]
    pub notification: u32,
}

pub fn load_net_config() -> Result<NetConfig, Box<dyn std::error::Error>> {
    let cfg = build_util::config_key::<GlobalConfig>("net")?.net;

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
    writeln!(out, "#[derive(serde::Serialize, serde::Deserialize)]")?;
    writeln!(out, "pub enum SocketName {{")?;
    for (i, s) in config.sockets.iter().enumerate() {
        writeln!(out, "    {} = {},", s.name, i)?;
    }
    writeln!(out, "}}")?;
    Ok(())
}
