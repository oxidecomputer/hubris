// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Result, bail};
use std::path::Path;
use std::process::Command;

/// Rewrites a file in-place using rustfmt.
///
/// Rustfmt likes to rewrite files in-place. If this concerns you, copy your
/// important file to a temporary file, and then call this on it.
pub fn rustfmt(path: impl AsRef<Path>) -> Result<()> {
    rustfmt_with_config(path, None::<&Path>)
}

pub fn rustfmt_with_config(
    path: impl AsRef<Path>,
    config: Option<impl AsRef<Path>>,
) -> Result<()> {
    let which_out =
        Command::new("rustup").args(["which", "rustfmt"]).output()?;

    if !which_out.status.success() {
        bail!("rustup which returned status {}", which_out.status);
    }

    let out_str = std::str::from_utf8(&which_out.stdout)?.trim();

    println!("will invoke: {out_str}");

    let mut cmd = Command::new(out_str);
    cmd.arg(path.as_ref());

    if let Some(p) = config {
        cmd.arg("--config-path").arg(p.as_ref());
    }

    let fmt_status = cmd.status()?;
    if !fmt_status.success() {
        bail!("rustfmt returned status {fmt_status}");
    }
    Ok(())
}
