// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Result, bail};
use build_i2c::{ConfigGenerator, Disposition};
use std::{fs::File, io::Write, path::Path};

use crate::config::Config;

fn disp_from_str(d: &str) -> Result<Disposition> {
    Ok(match d {
        // controller is an initiator
        "initiator" => Disposition::Initiator,
        // controller is a target
        "target" => Disposition::Target,
        // devices are used (i.e., controller is not used), but not as sensors
        "devices" => Disposition::Devices,
        // devices are used, with some used as sensors
        "sensors" => Disposition::Sensors,
        // devices are used, but only as validation
        "validation" => Disposition::Validation,
        // ???
        other => bail!("Unknown disposition: '{other}'"),
    })
}

pub fn run(
    cfg: &Path,
    disp: &str,
    output: Option<&Path>,
    fmt: bool,
) -> Result<()> {
    let disp = disp_from_str(disp)?;
    let cfg = Config::from_file(cfg)?;

    // This is dumb, but roughly approximates what we do in normal builds where
    // `xtask dist` will prepare the app toml and shove it in an env var, and
    // then i2c codegen will pull it from there.
    let config = toml::to_string(&cfg.config)?;
    let i2c_cfg: build_i2c::Config = build_util::toml_from_str(&config)?;
    let g = ConfigGenerator::new_with_config(disp.into(), i2c_cfg.i2c);

    let res = build_i2c::codegen_to_string_with_generator(g)?;

    if let Some(p) = output {
        let mut f = File::create(p)?;
        f.write_all(res.as_bytes())?;
        f.flush()?;
        drop(f);
        if fmt {
            call_rustfmt::rustfmt(p)?;
        }
    } else {
        assert!(!fmt, "--fmt only works with --output");
        println!("{res}");
    }

    Ok(())
}
