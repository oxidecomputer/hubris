// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::env;
use std::path::Path;
use std::process::Command;

use anyhow::Context;

use crate::{Config, HumilityArgs};

pub fn run(
    args: &HumilityArgs,
    precmd: &[&str],
    cmd: Option<&str>,
    interactive: bool,
    image_name: &String,
) -> anyhow::Result<()> {
    if interactive {
        ctrlc::set_handler(|| {}).expect("Error setting Ctrl-C handler");
    }
    let toml = Config::from_file(&args.cfg)?;

    let archive = Path::new("target")
        .join(&toml.name)
        .join("dist")
        .join(image_name)
        .join(toml.archive_name(image_name));

    let humility_path = match env::var("HUBRIS_HUMILITY_PATH") {
        Ok(path) => path,
        _ => "humility".to_string(),
    };

    let mut humility = Command::new(humility_path);
    humility.arg("-a").arg(archive);
    for c in precmd {
        humility.arg(c);
    }

    if let Some(cmd) = cmd {
        humility.arg(cmd);
    }

    for opt in &args.extra_options {
        humility.arg(opt);
    }

    let status = humility
        .status()
        .with_context(|| format!("failed to run humility ({humility:?})"))?;

    if !status.success() {
        anyhow::bail!("humility failed");
    }

    Ok(())
}
