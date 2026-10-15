// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::build_notifications()?;

    // The client works against either demo server, so rather than naming a
    // specific task, look up the `poke` notification of whichever task our
    // `server` slot points at.
    let me = build_util::task_full_config_toml()?;
    let server = me
        .task_slots
        .get("server")
        .ok_or("demo-client needs a `server` task slot")?;
    let poke = build_util::other_task_full_config_toml(server)?
        .notification_mask("poke")?;

    let out =
        std::path::Path::new(&std::env::var("OUT_DIR")?).join("server.rs");
    let mut out = std::fs::File::create(out)?;
    writeln!(out, "/// The server task's `poke` notification.")?;
    writeln!(out, "const SERVER_POKE_MASK: u32 = {poke:#x};")?;
    Ok(())
}
