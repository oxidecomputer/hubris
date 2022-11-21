// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::Path;

use anyhow::{bail, Context, Error, Result};

use crate::dist::PackageConfig;

pub fn run(
    cfg: &Path,
    archive: bool,
    image_name: Option<String>,
) -> Result<()> {
    if archive {
        let config = PackageConfig::new(cfg, false, false)
            .context("could not create build configuration")?;

        let image_name = image_name.unwrap_or(String::from("default"));

        let image_name = config
            .toml
            .image_names
            .iter()
            .find(|name| name == &&image_name)
            .ok_or(Error::msg(format!("cannot find image {}", image_name)))?;

        let final_path = config
            .img_file(format!("build-{}.zip", config.toml.name), image_name);

        println!("{}", final_path.display());
    } else {
        bail!("I'm not sure what to print. Currently supported: --archive");
    }

    Ok(())
}
