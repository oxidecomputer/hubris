// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::process::Command;
use std::{env, path::Path};

use anyhow::{bail, Result};

use crate::Config;

pub fn run(
    cfg: Option<&Path>,
    package: Option<String>,
    target: Option<String>,
) -> Result<()> {
    let package = package.unwrap_or_else(|| {
        let path = env::current_dir().unwrap();
        let manifest_path = path.join("Cargo.toml");
        let contents = std::fs::read(manifest_path).unwrap();
        let toml: toml::Value = toml::from_slice(&contents).unwrap();

        // someday, try blocks will be stable...
        toml.get("package")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .expect("Couldn't find [package.name]; pass -p <package> to run clippy on a specific package or --all to check all packages")
            .to_string()
    });

    // we could consider calling out to `cargo expand` if it exists, but for now
    // we'll just run the bare-bones rustc expansion without colorization, etc
    println!("Running macro expansion on: {}", package);

    let mut cmd = Command::new("cargo");
    cmd.arg("rustc");
    cmd.arg("--profile");
    cmd.arg("check");
    cmd.arg("-p");
    cmd.arg(&package);

    if let Some(target) = target {
        cmd.arg("--target");
        cmd.arg(target);
    }

    // macro expansion args
    cmd.arg("--");
    cmd.arg("-Zunpretty=expanded");

    // this is only actually used for demo-stm32h7 but is harmless to include,
    // so let's do it unconditionally
    cmd.env("HUBRIS_BOARD", "nucleo-h743zi2");

    // Expanding tasks that include build-time config from their app config
    // (e.g., lists of I2C devices) requires specifing the app config.
    if let Some(cfg) = cfg {
        let toml = Config::from_file(cfg)?;
        cmd.env("HUBRIS_APP_CONFIG", toml::to_string(&toml.config).unwrap());
    }

    let status = cmd.status()?;

    if !status.success() {
        bail!("Could not build {}", package);
    }

    Ok(())
}
