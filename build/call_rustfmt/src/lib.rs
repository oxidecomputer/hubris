// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::Path;
use std::process::Command;

/// Rewrites a file in-place using rustfmt.
///
/// Rustfmt likes to rewrite files in-place. If this concerns you, copy your
/// important file to a temporary file, and then call this on it.
pub fn rustfmt(
    path: impl AsRef<Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let which_out =
        Command::new("rustup").args(["which", "rustfmt"]).output()?;

    if !which_out.status.success() {
        return Err(format!(
            "rustup which returned status {}",
            which_out.status
        )
        .into());
    }

    let out_str = std::str::from_utf8(&which_out.stdout)?.trim();

    println!("will invoke: {}", out_str);

    let fmt_status = Command::new(out_str).arg(path.as_ref()).status()?;
    if !fmt_status.success() {
        return Err(format!("rustfmt returned status {}", fmt_status).into());
    }
    Ok(())
}
