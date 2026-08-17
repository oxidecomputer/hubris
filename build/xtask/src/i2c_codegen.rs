// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Result;
use build_i2c::{ConfigGenerator, Disposition};
use std::{fs::File, io::Write, path::Path};

use crate::config::Config;

/// Do I2C code generation
pub fn run(
    cfg: &Path,
    disp: Disposition,
    output: Option<&Path>,
    fmt: bool,
) -> Result<()> {
    let cfg = Config::from_file(cfg)?;

    // This is a little roundabout of a process, but roughly approximates what
    // we do in normal builds where `xtask dist` will prepare the app toml and
    // shove it in an env var, and then i2c codegen will pull it from there.
    //
    // We convert to a string using *xtask*'s notion of manifest tomls...
    let config = toml::to_string(&cfg.config)?;

    // ...and now that it's a string, parse the contents back as *i2c*'s
    // different notion of what a manifest toml looks like (mostly just the
    // i2c config section).
    let i2c_cfg: build_i2c::Config = build_util::toml_from_str(&config)?;
    let g = ConfigGenerator::new_with_config(disp.into(), i2c_cfg.i2c);

    // Do the codegen into a string
    let res = g.codegen_to_string()?;

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
