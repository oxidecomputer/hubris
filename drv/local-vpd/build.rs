// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::io::Write;

/// This represents our _subset_ of global config and _must not_ be marked with
/// `deny_unknown_fields`!
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GlobalConfig {
    pub local_vpd: LocalVpdConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LocalVpdConfig {
    /// Sockets known to the system, indexed by name.
    pub vpd_bus: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = build_util::config::<GlobalConfig>()?.local_vpd;

    build_i2c::codegen(build_i2c::Disposition::Devices)?;

    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path = std::path::Path::new(&out_dir).join("vpd_config.rs");
    let mut out = std::fs::File::create(&dest_path)?;

    write!(
        out,
        r#"
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
pub fn get_vpd_eeprom(i2c_task: userlib::TaskId)
    -> drv_i2c_devices::at24csw080::At24Csw080
{{
    let devs = i2c_config::devices::at24csw080_{}(i2c_task);
    assert_eq!(devs.len(), 1);
    drv_i2c_devices::at24csw080::At24Csw080::new(devs[0])
}}
"#,
        cfg.vpd_bus
    )?;
    Ok(())
}
